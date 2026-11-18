//! Записи приложения в базе (признак `store`): журнал «вид, номер, содержимое» и вложения.
//!
//! Каждое изменение — событие с номером от устройства и часами Лэмпорта. Таблица `items` собирается
//! из событий: при споре двух устройств побеждает более позднее изменение (по часам Лэмпорта, при
//! равенстве — по номеру устройства). Тело события — `{"op":"put","kind":…,"id":…,"data":…}` или
//! `{"op":"del","kind":…,"id":…}`: так журнал ведёт «Полка», и её база открывается этим хранилищем
//! как есть. Вложения лежат по SHA-256 содержимого; какие подходят и в каких полях записей на них
//! ссылаются, приложение описывает в [`Blobs`].
//!
//! Хранилище само отвечает обмену по Wi-Fi ([`crate::sync::Journal`], [`crate::lan::Device`]) и облаку
//! ([`crate::cloud::CloudStore`]): новому приложению на kit остаётся описать свои записи. Свои таблицы
//! приложение заводит в той же базе через [`Store::conn`] — в журнал они не попадают.

use crate::cloud::blob_id;
use crate::db;
use crate::error::{Error, Result};
use crate::sync::{NoFile, VersionVector};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Вложения приложения: какие подходят и где записи на них ссылаются.
pub struct Blobs {
    /// Наибольший размер вложения, байт.
    pub max: usize,
    /// Подходит ли вложение по типу и первым байтам: под видом снимка не пролезет что-то другое.
    pub check: fn(&str, &[u8]) -> bool,
    /// Поля записей со ссылкой на вложение — пути JSON для SQLite, например `$.cover`.
    pub refs: &'static [&'static str],
}

fn no_blob(_mime: &str, _data: &[u8]) -> bool {
    false
}

/// Приложение без вложений: записи только в журнале.
pub const NO_BLOBS: Blobs = Blobs { max: 0, check: no_blob, refs: &[] };

/// Тело события журнала.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum EventBody {
    Put { kind: String, id: String, data: serde_json::Value },
    Del { kind: String, id: String },
}

/// Событие журнала: устройство, его порядковый номер, часы Лэмпорта и что изменилось.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub device: String,
    pub seq: i64,
    pub lamport: i64,
    pub body: EventBody,
}

/// Зашифрованная база приложения с журналом записей.
pub struct Store {
    conn: Connection,
    device: String,
    blobs: &'static Blobs,
}

fn db_err(e: rusqlite::Error) -> Error {
    Error::Store(e.to_string())
}

fn bad(text: &str) -> Error {
    Error::Store(text.to_string())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Store {
    /// Открывает или создаёт базу, зашифрованную 256-битным ключом. `device` — номер этого устройства.
    pub fn open(path: &Path, key: &[u8; 32], device: &str, blobs: &'static Blobs) -> std::result::Result<Store, db::Open> {
        let conn = db::open(path, key)?;
        let store = Store { conn, device: device.to_string(), blobs };
        if db::meta(&store.conn, "schema")?.is_none() {
            db::set_meta(&store.conn, "schema", "1")?;
        }
        Ok(store)
    }

    /// Соединение с базой — для своих таблиц приложения (в журнал и обмен они не попадают).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Соединение для транзакций над своими таблицами приложения.
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    /// Местное значение устройства (в журнал не попадает).
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        db::meta(&self.conn, key).map_err(db_err)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        db::set_meta(&self.conn, key, value).map_err(db_err)
    }

    pub fn delete_meta(&self, key: &str) -> Result<()> {
        db::delete_meta(&self.conn, key).map_err(db_err)
    }

    /// Местные значения, чьи ключи начинаются с `prefix`.
    pub fn meta_prefix(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        db::meta_prefix(&self.conn, prefix).map_err(db_err)
    }

    fn record(&mut self, body: EventBody) -> Result<()> {
        let device = self.device.clone();
        let tx = self.conn.transaction().map_err(db_err)?;
        let (seq, lamport) = db::next(&tx, &device).map_err(db_err)?;
        let ev = Event { device, seq, lamport, body };
        db::insert_event(&tx, &ev.device, ev.seq, ev.lamport, &serde_json::to_string(&ev.body)?).map_err(db_err)?;
        apply(&tx, &ev)?;
        tx.commit().map_err(db_err)
    }

    /// Записать значение (новое или изменённое): событие в журнал и запись в таблицу.
    pub fn put<T: Serialize>(&mut self, kind: &str, id: &str, value: &T) -> Result<()> {
        if kind.is_empty() || id.is_empty() {
            return Err(bad("пустой вид или номер записи"));
        }
        let data = serde_json::to_value(value)?;
        self.record(EventBody::Put { kind: kind.into(), id: id.into(), data })
    }

    pub fn delete(&mut self, kind: &str, id: &str) -> Result<()> {
        if kind.is_empty() || id.is_empty() {
            return Err(bad("пустой вид или номер записи"));
        }
        self.record(EventBody::Del { kind: kind.into(), id: id.into() })
    }

    pub fn get<T: DeserializeOwned>(&self, kind: &str, id: &str) -> Result<Option<T>> {
        let body: Option<Option<String>> = self
            .conn
            .query_row("SELECT body FROM items WHERE kind = ?1 AND id = ?2 AND deleted = 0", params![kind, id], |r| r.get(0))
            .optional()
            .map_err(db_err)?;
        match body.flatten() {
            Some(b) => Ok(Some(serde_json::from_str(&b)?)),
            None => Ok(None),
        }
    }

    /// Все живые записи вида, по номеру. Запись, которую эта версия не понимает, пропускается, а не роняет список.
    pub fn list<T: DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>> {
        let mut st = self.conn.prepare("SELECT body FROM items WHERE kind = ?1 AND deleted = 0 AND body IS NOT NULL ORDER BY id").map_err(db_err)?;
        let rows = st.query_map([kind], |r| r.get::<_, String>(0)).map_err(db_err)?;
        let mut out = Vec::new();
        for b in rows {
            if let Ok(v) = serde_json::from_str(&b.map_err(db_err)?) {
                out.push(v);
            }
        }
        Ok(out)
    }

    /// Сохранить вложение; номер — SHA-256 содержимого. То же вложение второй раз не пишется.
    pub fn put_blob(&mut self, mime: &str, data: &[u8]) -> Result<String> {
        if data.is_empty() || data.len() > self.blobs.max {
            return Err(Error::Store(format!("вложение пустое или больше {} КБ", self.blobs.max / 1024)));
        }
        if !(self.blobs.check)(mime, data) {
            return Err(bad("такое вложение не подходит"));
        }
        let id = blob_id(data);
        db::insert_blob(&self.conn, &id, mime, data, now_secs()).map_err(db_err)?;
        Ok(id)
    }

    pub fn blob(&self, id: &str) -> Result<Option<(String, Vec<u8>)>> {
        db::blob(&self.conn, id).map_err(db_err)
    }

    pub fn has_blob(&self, id: &str) -> Result<bool> {
        db::has_blob(&self.conn, id).map_err(db_err)
    }

    /// Вложение под известным номером (из копии или с другого устройства): номер должен совпасть
    /// с отпечатком содержимого.
    pub fn put_blob_checked(&mut self, id: &str, mime: &str, data: &[u8]) -> Result<()> {
        if blob_id(data) != id {
            return Err(bad("вложение не совпадает со своим номером"));
        }
        self.put_blob(mime, data).map(|_| ())
    }

    /// Номера всех вложений в базе.
    pub fn present_blobs(&self) -> Result<Vec<String>> {
        db::blob_ids(&self.conn).map_err(db_err)
    }

    /// Номера вложений, на которые ссылаются живые записи (поля из [`Blobs::refs`]).
    pub fn referenced_blobs(&self) -> Result<BTreeSet<String>> {
        let mut out = BTreeSet::new();
        for path in self.blobs.refs {
            let mut st = self.conn.prepare("SELECT json_extract(body, ?1) FROM items WHERE deleted = 0 AND body IS NOT NULL").map_err(db_err)?;
            let rows = st.query_map([path], |r| r.get::<_, Option<String>>(0)).map_err(db_err)?;
            for r in rows {
                if let Some(id) = r.map_err(db_err)? {
                    out.insert(id);
                }
            }
        }
        Ok(out)
    }

    /// Вложения, на которые ссылаются записи, но которых нет (ещё не пришли с другого устройства).
    pub fn missing_blobs(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for id in self.referenced_blobs()? {
            if !self.has_blob(&id)? {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Сколько событий в журнале.
    pub fn event_count(&self) -> Result<u64> {
        db::event_count(&self.conn).map_err(db_err)
    }

    /// Событие устройства под номером — чтобы сверить с базой копию или облако.
    pub fn event_body(&self, device: &str, seq: i64) -> Result<Option<EventBody>> {
        Ok(match db::event_body(&self.conn, device, seq).map_err(db_err)? {
            Some(b) => Some(serde_json::from_str(&b)?),
            None => None,
        })
    }

    /// Последний номер события каждого устройства — что эта база уже знает.
    pub fn versions(&self) -> Result<BTreeMap<String, i64>> {
        db::versions(&self.conn).map_err(db_err)
    }

    /// События, которых нет у другого устройства (по его `versions`), в порядке часов Лэмпорта.
    pub fn events_missing(&self, known: &BTreeMap<String, i64>) -> Result<Vec<Event>> {
        let mut out = Vec::new();
        for r in db::events_by_lamport(&self.conn).map_err(db_err)? {
            if known.get(&r.device).is_some_and(|k| *k >= r.seq) {
                continue;
            }
            out.push(event(r)?);
        }
        Ok(out)
    }

    /// События одного устройства с номерами from..=to, по порядку.
    pub fn events_range(&self, device: &str, from: i64, to: i64) -> Result<Vec<Event>> {
        db::events_range(&self.conn, device, from, to).map_err(db_err)?.into_iter().map(event).collect()
    }

    /// Принять события другого устройства. Уже известные пропускаются. Возвращает число новых.
    pub fn apply_remote(&mut self, events: &[Event]) -> Result<usize> {
        let tx = self.conn.transaction().map_err(db_err)?;
        let mut added = 0;
        for ev in events {
            if !db::insert_event(&tx, &ev.device, ev.seq, ev.lamport, &serde_json::to_string(&ev.body)?).map_err(db_err)? {
                continue;
            }
            apply(&tx, ev)?;
            added += 1;
        }
        tx.commit().map_err(db_err)?;
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
    let current: Option<(i64, String)> = tx
        .query_row("SELECT lamport, device FROM items WHERE kind = ?1 AND id = ?2", params![kind, id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(db_err)?;
    if let Some((lamport, device)) = current {
        if (lamport, device.as_str()) > (ev.lamport, ev.device.as_str()) {
            return Ok(());
        }
    }
    tx.execute(
        "INSERT INTO items(kind, id, body, deleted, lamport, device) VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(kind, id) DO UPDATE SET body = excluded.body, deleted = excluded.deleted,
           lamport = excluded.lamport, device = excluded.device",
        params![kind, id, body, deleted, ev.lamport, ev.device],
    )
    .map_err(db_err)?;
    Ok(())
}

/// Журнал для обмена по Wi-Fi и через облако: события и вложения; большие файлы кусками — нет.
impl crate::sync::Journal for Store {
    type Event = Event;
    type File = NoFile;

    fn device(&self) -> String {
        self.device.clone()
    }
    fn versions(&self) -> Result<VersionVector> {
        Ok(Store::versions(self)?.into_iter().map(|(d, s)| (d, s.max(0) as u64)).collect())
    }
    fn events_missing(&self, theirs: &VersionVector) -> Result<Vec<Event>> {
        let theirs: BTreeMap<String, i64> = theirs.iter().map(|(d, s)| (d.clone(), (*s).min(i64::MAX as u64) as i64)).collect();
        Store::events_missing(self, &theirs)
    }
    fn apply_remote(&mut self, events: &[Event]) -> Result<usize> {
        Store::apply_remote(self, events)
    }
    fn missing_blobs(&self) -> Result<Vec<String>> {
        Store::missing_blobs(self)
    }
    fn blob(&self, id: &str) -> Result<Option<(String, Vec<u8>)>> {
        Store::blob(self, id)
    }
    fn put_blob_checked(&mut self, id: &str, mime: &str, data: &[u8]) -> Result<()> {
        Store::put_blob_checked(self, id, mime, data)
    }
}

/// Чего облаку нужно сверх обмена между устройствами.
impl crate::cloud::CloudStore for Store {
    fn origin(e: &Event) -> (&str, u64) {
        (&e.device, e.seq.max(0) as u64)
    }
    fn events_range(&self, author: &str, from: u64, to: u64) -> Result<Vec<Event>> {
        let to = to.min(i64::MAX as u64) as i64;
        let from = from.min(i64::MAX as u64) as i64;
        Store::events_range(self, author, from, to)
    }
    fn differs(&self, e: &Event) -> Result<bool> {
        Ok(Store::event_body(self, &e.device, e.seq)?.is_some_and(|b| b != e.body))
    }
    fn present_blobs(&self) -> Result<Vec<String>> {
        Store::present_blobs(self)
    }
}

/// Связь по Wi-Fi в приложении: связанные устройства — в местных значениях, число событий — из журнала.
impl crate::lan::Device for Store {
    fn local(&self, key: &str) -> Option<String> {
        self.meta(key).ok().flatten()
    }
    fn set_local(&self, key: &str, value: &str) -> Result<()> {
        self.set_meta(key, value)
    }
    fn event_count(&self) -> u64 {
        Store::event_count(self).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::Journal;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Note {
        id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        photo: Option<String>,
    }

    /// Временная папка проверки; удаляется после неё.
    struct Tmp(std::path::PathBuf);

    impl Tmp {
        fn new() -> Tmp {
            let mut b = [0u8; 8];
            getrandom::fill(&mut b).unwrap();
            let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
            let d = std::env::temp_dir().join(format!("kit-store-{hex}"));
            std::fs::create_dir_all(&d).unwrap();
            Tmp(d)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn note(id: &str, text: &str) -> Note {
        Note { id: id.into(), text: text.into(), photo: None }
    }

    fn jpeg_ok(mime: &str, data: &[u8]) -> bool {
        mime == "image/jpeg" && data.starts_with(&[0xFF, 0xD8, 0xFF])
    }

    static PHOTOS: Blobs = Blobs { max: 1024 * 1024, check: jpeg_ok, refs: &["$.photo"] };

    fn jpeg(fill: u8) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0];
        v.extend(std::iter::repeat_n(fill, 100));
        v
    }

    fn open(dir: &Path, name: &str, device: &str) -> Store {
        Store::open(&dir.join(name), &[7u8; 32], device, &PHOTOS).unwrap()
    }

    #[test]
    fn put_get_list_delete() {
        let dir = Tmp::new();
        let mut s = open(dir.path(), "a.db", "aaaa");
        s.put("note", "2", &note("2", "второй")).unwrap();
        s.put("note", "1", &note("1", "первый")).unwrap();
        assert_eq!(s.get::<Note>("note", "1").unwrap(), Some(note("1", "первый")));
        let all: Vec<Note> = s.list("note").unwrap();
        assert_eq!(all.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["1", "2"]);
        s.put("note", "1", &note("1", "исправлен")).unwrap();
        assert_eq!(s.get::<Note>("note", "1").unwrap().unwrap().text, "исправлен");
        s.delete("note", "1").unwrap();
        assert_eq!(s.get::<Note>("note", "1").unwrap(), None);
        assert_eq!(s.list::<Note>("note").unwrap().len(), 1);
        assert_eq!(Store::event_count(&s).unwrap(), 4);
        assert!(s.put("", "x", &1).is_err());
        assert!(s.delete("note", "").is_err());
    }

    #[test]
    fn records_this_version_cannot_read_are_skipped() {
        let dir = Tmp::new();
        let mut s = open(dir.path(), "a.db", "aaaa");
        s.put("note", "1", &note("1", "есть")).unwrap();
        s.put("note", "2", &serde_json::json!({"другое": true})).unwrap();
        assert_eq!(s.list::<Note>("note").unwrap(), vec![note("1", "есть")]);
    }

    #[test]
    fn local_values_by_prefix_stay_out_of_the_journal() {
        let dir = Tmp::new();
        let s = open(dir.path(), "a.db", "aaaa");
        s.set_meta("lan.peers", "[]").unwrap();
        s.set_meta("lan.code", "x").unwrap();
        s.set_meta("cloud.every", "auto").unwrap();
        assert_eq!(s.meta_prefix("lan.").unwrap().len(), 2);
        s.delete_meta("lan.code").unwrap();
        assert_eq!(s.meta("lan.code").unwrap(), None);
        assert_eq!(Store::event_count(&s).unwrap(), 0);
    }

    #[test]
    fn file_is_encrypted_and_needs_its_key() {
        let dir = Tmp::new();
        let path = dir.path().join("a.db");
        {
            let mut s = Store::open(&path, &[1u8; 32], "aaaa", &NO_BLOBS).unwrap();
            s.put("note", "1", &note("1", "секретная строка")).unwrap();
        }
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(16).any(|w| w == "секретная строка".as_bytes().get(..16).unwrap()));
        assert!(matches!(Store::open(&path, &[2u8; 32], "aaaa", &NO_BLOBS), Err(db::Open::WrongKey)));
        assert!(Store::open(&path, &[1u8; 32], "aaaa", &NO_BLOBS).is_ok());
    }

    /// Тело события — то же, что журнал «Полки»: её база открывается хранилищем как есть.
    #[test]
    fn event_bodies_are_the_same_as_polka() {
        let put = EventBody::Put { kind: "title".into(), id: "ab12".into(), data: serde_json::json!({"title": "Х"}) };
        assert_eq!(serde_json::to_string(&put).unwrap(), r#"{"op":"put","kind":"title","id":"ab12","data":{"title":"Х"}}"#);
        let del = EventBody::Del { kind: "title".into(), id: "ab12".into() };
        assert_eq!(serde_json::to_string(&del).unwrap(), r#"{"op":"del","kind":"title","id":"ab12"}"#);
        let back: EventBody = serde_json::from_str(r#"{"op":"put","kind":"setting","id":"view","data":"shelf"}"#).unwrap();
        assert_eq!(back, EventBody::Put { kind: "setting".into(), id: "view".into(), data: serde_json::json!("shelf") });
    }

    #[test]
    fn two_devices_converge_and_later_edit_wins() {
        let dir = Tmp::new();
        let mut a = open(dir.path(), "a.db", "aaaa");
        let mut b = open(dir.path(), "b.db", "bbbb");
        a.put("note", "1", &note("1", "с телефона")).unwrap();
        b.put("note", "2", &note("2", "с компьютера")).unwrap();
        let to_b = a.events_missing(&b.versions().unwrap()).unwrap();
        assert_eq!(b.apply_remote(&to_b).unwrap(), 1);
        let to_a = b.events_missing(&a.versions().unwrap()).unwrap();
        assert_eq!(a.apply_remote(&to_a).unwrap(), 1);
        // Повтор ничего не меняет.
        assert_eq!(a.apply_remote(&to_a).unwrap(), 0);
        // Обе правят одну запись: позже — по часам Лэмпорта — правка, сделанная после обмена.
        a.put("note", "1", &note("1", "раньше")).unwrap();
        b.put("note", "1", &note("1", "позже")).unwrap();
        b.put("note", "1", &note("1", "позже ещё")).unwrap();
        let to_b = a.events_missing(&b.versions().unwrap()).unwrap();
        let to_a = b.events_missing(&a.versions().unwrap()).unwrap();
        b.apply_remote(&to_b).unwrap();
        a.apply_remote(&to_a).unwrap();
        let on_a: Vec<Note> = a.list("note").unwrap();
        let on_b: Vec<Note> = b.list("note").unwrap();
        assert_eq!(on_a, on_b);
        assert_eq!(on_a[0].text, "позже ещё");
        // Удаление тоже доходит.
        a.delete("note", "2").unwrap();
        let to_b = a.events_missing(&b.versions().unwrap()).unwrap();
        b.apply_remote(&to_b).unwrap();
        assert_eq!(b.get::<Note>("note", "2").unwrap(), None);
    }

    #[test]
    fn journal_trait_speaks_unsigned_versions() {
        let dir = Tmp::new();
        let mut a = open(dir.path(), "a.db", "aaaa");
        a.put("note", "1", &note("1", "x")).unwrap();
        let v: VersionVector = Journal::versions(&a).unwrap();
        assert_eq!(v.get("aaaa"), Some(&1));
        assert_eq!(Journal::events_missing(&a, &v).unwrap().len(), 0);
        assert_eq!(Journal::events_missing(&a, &VersionVector::new()).unwrap().len(), 1);
        assert_eq!(Journal::device(&a), "aaaa");
        use crate::cloud::CloudStore;
        let evs = CloudStore::events_range(&a, "aaaa", 1, 1).unwrap();
        assert_eq!(<Store as CloudStore>::origin(&evs[0]), ("aaaa", 1));
        assert!(!a.differs(&evs[0]).unwrap());
        let mut changed = evs[0].clone();
        changed.body = EventBody::Del { kind: "note".into(), id: "1".into() };
        assert!(a.differs(&changed).unwrap());
    }

    #[test]
    fn blobs_follow_the_records_that_point_at_them() {
        let dir = Tmp::new();
        let mut a = open(dir.path(), "a.db", "aaaa");
        let mut b = open(dir.path(), "b.db", "bbbb");
        let data = jpeg(3);
        let id = a.put_blob("image/jpeg", &data).unwrap();
        assert_eq!(a.put_blob("image/jpeg", &data).unwrap(), id);
        assert!(a.put_blob("image/png", &data).is_err());
        assert!(a.put_blob("image/jpeg", &[]).is_err());
        let mut n = note("1", "со снимком");
        n.photo = Some(id.clone());
        a.put("note", "1", &n).unwrap();
        assert!(a.missing_blobs().unwrap().is_empty());
        b.apply_remote(&a.events_missing(&b.versions().unwrap()).unwrap()).unwrap();
        assert_eq!(b.missing_blobs().unwrap(), vec![id.clone()]);
        assert!(b.put_blob_checked(&id, "image/jpeg", &jpeg(4)).is_err());
        b.put_blob_checked(&id, "image/jpeg", &data).unwrap();
        assert!(b.missing_blobs().unwrap().is_empty());
        assert_eq!(b.blob(&id).unwrap().unwrap().1, data);
        assert_eq!(b.present_blobs().unwrap(), vec![id]);
    }

    #[test]
    fn app_tables_live_next_to_the_journal() {
        let dir = Tmp::new();
        let mut s = open(dir.path(), "a.db", "aaaa");
        s.conn().execute_batch("CREATE TABLE IF NOT EXISTS mail(id TEXT PRIMARY KEY, subject TEXT)").unwrap();
        let tx = s.conn_mut().transaction().unwrap();
        tx.execute("INSERT INTO mail VALUES('1', 'Привет')", []).unwrap();
        tx.commit().unwrap();
        let subject: String = s.conn().query_row("SELECT subject FROM mail WHERE id = '1'", [], |r| r.get(0)).unwrap();
        assert_eq!(subject, "Привет");
        assert_eq!(Store::event_count(&s).unwrap(), 0);
    }
}
