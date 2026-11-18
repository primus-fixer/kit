//! Зашифрованная база (SQLCipher) с журналом событий.
//!
//! Открытие базы, таблицы, местные значения (`meta`), выборки журнала и снимки — общие, из kit
//! (`kit_core::db`). Здесь — тело события: записи «вид, номер, содержимое» (как у «Полки»). Так новый вид
//! записей не требует новой версии базы: это просто другой `kind`. Каждое изменение — событие с номером
//! от устройства и часами Лэмпорта; таблица записей собирается из событий, при споре двух устройств
//! побеждает более позднее изменение (по часам Лэмпорта, при равенстве — по номеру устройства).
//!
//! Снимки (картинки) лежат отдельно, по SHA-256 содержимого, и в журнал не попадают. Запись ссылается
//! на снимок полем `image` — так обмен узнаёт, каких снимков не хватает.

use crate::error::{bad, Error, Result};
use kit_core::db;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Снимок — не больше 8 МБ.
pub const MAX_BLOB: usize = 8 * 1024 * 1024;

/// Номер снимка — SHA-256 содержимого.
pub use kit_core::cloud::blob_id;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum EventBody {
    Put { kind: String, id: String, data: serde_json::Value },
    Del { kind: String, id: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub device: String,
    pub seq: i64,
    pub lamport: i64,
    pub body: EventBody,
}

/// Тип картинки по первым байтам: под видом снимка не пролезет что-то другое.
pub fn sniff_image(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Снимок подходит для базы: не пустой, не больше предела, тип совпадает с первыми байтами.
pub fn check_blob(mime: &str, data: &[u8]) -> Result<()> {
    if data.is_empty() || data.len() > MAX_BLOB {
        return Err(bad(format!("снимок пустой или больше {} МБ", MAX_BLOB / 1024 / 1024)));
    }
    if sniff_image(data) != Some(mime) {
        return Err(bad("подходят только снимки JPEG, PNG и WebP"));
    }
    Ok(())
}

pub struct Store {
    conn: Connection,
    device: String,
}

impl Store {
    /// Открывает или создаёт базу, зашифрованную 256-битным ключом.
    pub fn open(path: &Path, key: &[u8; 32], device: &str) -> Result<Store> {
        let conn = db::open(path, key).map_err(|e| match e {
            db::Open::WrongKey => Error::WrongKey,
            db::Open::Db(e) => e.into(),
        })?;
        let store = Store { conn, device: device.to_string() };
        if store.meta("schema")?.is_none() {
            store.set_meta("schema", "1")?;
        }
        Ok(store)
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    // ——— Местные значения устройства (в журнал не попадают) ———

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(db::meta(&self.conn, key)?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        Ok(db::set_meta(&self.conn, key, value)?)
    }

    pub fn delete_meta(&self, key: &str) -> Result<()> {
        Ok(db::delete_meta(&self.conn, key)?)
    }

    // ——— Записи ———

    fn record(&mut self, body: EventBody) -> Result<()> {
        let tx = self.conn.transaction()?;
        let (seq, lamport) = db::next(&tx, &self.device)?;
        let ev = Event { device: self.device.clone(), seq, lamport, body };
        db::insert_event(&tx, &ev.device, ev.seq, ev.lamport, &serde_json::to_string(&ev.body)?)?;
        apply(&tx, &ev)?;
        tx.commit()?;
        Ok(())
    }

    /// Записать значение (новое или изменённое).
    pub fn put<T: Serialize>(&mut self, kind: &str, id: &str, value: &T) -> Result<()> {
        if kind.is_empty() || id.is_empty() {
            return Err(bad("пустой вид или номер записи"));
        }
        let data = serde_json::to_value(value)?;
        self.record(EventBody::Put { kind: kind.into(), id: id.into(), data })
    }

    pub fn delete(&mut self, kind: &str, id: &str) -> Result<()> {
        self.record(EventBody::Del { kind: kind.into(), id: id.into() })
    }

    pub fn get<T: DeserializeOwned>(&self, kind: &str, id: &str) -> Result<Option<T>> {
        let body: Option<Option<String>> =
            self.conn.query_row("SELECT body FROM items WHERE kind = ?1 AND id = ?2 AND deleted = 0", params![kind, id], |r| r.get(0)).optional()?;
        match body.flatten() {
            Some(b) => Ok(Some(serde_json::from_str(&b)?)),
            None => Ok(None),
        }
    }

    /// Все живые записи вида. Запись, которую эта версия не понимает, пропускается, а не роняет список.
    pub fn list<T: DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>> {
        let mut st = self.conn.prepare("SELECT body FROM items WHERE kind = ?1 AND deleted = 0 AND body IS NOT NULL ORDER BY id")?;
        let rows = st.query_map([kind], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for b in rows {
            if let Ok(v) = serde_json::from_str(&b?) {
                out.push(v);
            }
        }
        Ok(out)
    }

    // ——— Снимки ———

    /// Сохранить снимок. Тот же снимок второй раз не пишется.
    pub fn put_blob(&mut self, mime: &str, data: &[u8]) -> Result<String> {
        check_blob(mime, data)?;
        let id = blob_id(data);
        db::insert_blob(&self.conn, &id, mime, data, crate::now_secs())?;
        Ok(id)
    }

    /// Снимок под известным номером (с другого устройства или из копии): номер должен совпасть с содержимым.
    pub fn put_blob_checked(&mut self, id: &str, mime: &str, data: &[u8]) -> Result<()> {
        if blob_id(data) != id {
            return Err(bad("снимок не совпадает со своим номером"));
        }
        self.put_blob(mime, data).map(|_| ())
    }

    pub fn blob(&self, id: &str) -> Result<Option<(String, Vec<u8>)>> {
        Ok(db::blob(&self.conn, id)?)
    }

    pub fn has_blob(&self, id: &str) -> Result<bool> {
        Ok(db::has_blob(&self.conn, id)?)
    }

    /// Номера всех снимков в базе.
    pub fn present_blobs(&self) -> Result<Vec<String>> {
        Ok(db::blob_ids(&self.conn)?)
    }

    /// Номера снимков, на которые ссылаются живые записи (поле `image`).
    pub fn referenced_blobs(&self) -> Result<BTreeSet<String>> {
        let mut st = self.conn.prepare("SELECT json_extract(body, '$.image') FROM items WHERE deleted = 0 AND body IS NOT NULL")?;
        let rows = st.query_map([], |r| r.get::<_, Option<String>>(0))?;
        let mut out = BTreeSet::new();
        for r in rows {
            if let Some(id) = r? {
                out.insert(id);
            }
        }
        Ok(out)
    }

    /// Снимки, на которые ссылаются записи, но самих снимков нет (ещё не пришли с другого устройства).
    pub fn missing_blobs(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for id in self.referenced_blobs()? {
            if !self.has_blob(&id)? {
                out.push(id);
            }
        }
        Ok(out)
    }

    // ——— Журнал для обмена ———

    /// Сколько событий в журнале.
    pub fn event_count(&self) -> Result<u64> {
        Ok(db::event_count(&self.conn)?)
    }

    /// Событие устройства под номером.
    pub fn event_body(&self, device: &str, seq: i64) -> Result<Option<EventBody>> {
        Ok(match db::event_body(&self.conn, device, seq)? {
            Some(b) => Some(serde_json::from_str(&b)?),
            None => None,
        })
    }

    /// Последний номер события каждого устройства — что эта база уже знает.
    pub fn versions(&self) -> Result<BTreeMap<String, i64>> {
        Ok(db::versions(&self.conn)?)
    }

    /// События, которых нет у другого устройства (по его `versions`).
    pub fn events_missing(&self, known: &BTreeMap<String, i64>) -> Result<Vec<Event>> {
        let mut out = Vec::new();
        for r in db::events_by_lamport(&self.conn)? {
            if known.get(&r.device).is_some_and(|k| *k >= r.seq) {
                continue;
            }
            out.push(event(r)?);
        }
        Ok(out)
    }

    /// События одного устройства с номерами from..=to, по порядку.
    pub fn events_range(&self, device: &str, from: i64, to: i64) -> Result<Vec<Event>> {
        db::events_range(&self.conn, device, from, to)?.into_iter().map(event).collect()
    }

    /// Принять события другого устройства. Уже известные пропускаются. Возвращает число новых.
    pub fn apply_remote(&mut self, events: &[Event]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut added = 0;
        for ev in events {
            if !db::insert_event(&tx, &ev.device, ev.seq, ev.lamport, &serde_json::to_string(&ev.body)?)? {
                continue;
            }
            apply(&tx, ev)?;
            added += 1;
        }
        tx.commit()?;
        Ok(added)
    }
}

/// Событие журнала из строки базы.
fn event(r: db::Row) -> Result<Event> {
    Ok(Event { device: r.device, seq: r.seq, lamport: r.lamport, body: serde_json::from_str(&r.body)? })
}

/// Применить событие к таблице записей: побеждает более позднее (часы Лэмпорта, затем устройство).
fn apply(tx: &Transaction, ev: &Event) -> Result<()> {
    let (kind, id, body, deleted) = match &ev.body {
        EventBody::Put { kind, id, data } => (kind, id, Some(data.to_string()), 0),
        EventBody::Del { kind, id } => (kind, id, None, 1),
    };
    let cur: Option<(i64, String)> =
        tx.query_row("SELECT lamport, device FROM items WHERE kind = ?1 AND id = ?2", params![kind, id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    if let Some((lamport, device)) = cur {
        if (lamport, device.as_str()) > (ev.lamport, ev.device.as_str()) {
            return Ok(());
        }
    }
    tx.execute(
        "INSERT INTO items(kind, id, body, deleted, lamport, device) VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(kind, id) DO UPDATE SET body = excluded.body, deleted = excluded.deleted,
           lamport = excluded.lamport, device = excluded.device",
        params![kind, id, body, deleted, ev.lamport, ev.device],
    )?;
    Ok(())
}
