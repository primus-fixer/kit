//! База приложения (SQLCipher): общая часть хранилища. Признак `db`.
//!
//! Таблицы у приложений на kit одни и те же: `meta` — местные значения устройства (в журнал не попадают),
//! `events` — журнал событий с номером от устройства и часами Лэмпорта, `items` — записи, собранные из
//! журнала, `blobs` — вложения по SHA-256 содержимого. Здесь — открытие базы ключом, `meta`, выборки журнала
//! для обмена и вложения. Что лежит в теле события и как оно превращается в запись, решает приложение.
//!
//! **Таблицы и их поля не меняются:** базы, созданные приложениями до kit, открываются как раньше.

use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeMap;
use std::path::Path;

/// Таблицы базы — те же, что у «Кармана» и «Полки» до kit.
const TABLES: &str = "PRAGMA journal_mode = WAL;
     CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS events(
       device TEXT NOT NULL, seq INTEGER NOT NULL, lamport INTEGER NOT NULL, body TEXT NOT NULL,
       PRIMARY KEY(device, seq));
     CREATE TABLE IF NOT EXISTS items(
       kind TEXT NOT NULL, id TEXT NOT NULL, body TEXT, deleted INTEGER NOT NULL DEFAULT 0,
       lamport INTEGER NOT NULL, device TEXT NOT NULL,
       PRIMARY KEY(kind, id));
     CREATE TABLE IF NOT EXISTS blobs(
       id TEXT PRIMARY KEY, mime TEXT NOT NULL, size INTEGER NOT NULL, data BLOB NOT NULL,
       added INTEGER NOT NULL);";

/// Почему база не открылась.
#[derive(Debug)]
pub enum Open {
    /// Ключ не подходит (или файл — не база).
    WrongKey,
    /// Ошибка SQLite: файл не открылся, диск, права.
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for Open {
    fn from(e: rusqlite::Error) -> Self {
        Open::Db(e)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Открывает или создаёт базу, зашифрованную 256-битным ключом, и создаёт недостающие таблицы.
/// Строка с ключом после использования затирается.
pub fn open(path: &Path, key: &[u8; 32]) -> Result<Connection, Open> {
    let conn = Connection::open(path)?;
    let mut pragma = format!("PRAGMA key = \"x'{}'\";", hex(key)).into_bytes();
    let keyed = conn.execute_batch(std::str::from_utf8(&pragma).unwrap_or_default());
    pragma.fill(0);
    std::hint::black_box(&pragma);
    keyed?;
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).map_err(|_| Open::WrongKey)?;
    conn.execute_batch(TABLES)?;
    Ok(conn)
}

/// Местное значение устройства.
pub fn meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute("INSERT INTO meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value])?;
    Ok(())
}

pub fn delete_meta(conn: &Connection, key: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM meta WHERE key = ?1", [key])?;
    Ok(())
}

/// Местные значения, чьи ключи начинаются с `prefix`, по порядку ключей.
pub fn meta_prefix(conn: &Connection, prefix: &str) -> rusqlite::Result<Vec<(String, String)>> {
    let mut st = conn.prepare("SELECT key, value FROM meta WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key")?;
    let rows = st.query_map([prefix], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    rows.collect()
}

/// Событие журнала как оно лежит в базе: тело — строка JSON приложения.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub device: String,
    pub seq: i64,
    pub lamport: i64,
    pub body: String,
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row { device: r.get(0)?, seq: r.get(1)?, lamport: r.get(2)?, body: r.get(3)? })
}

/// Сколько событий в журнале.
pub fn event_count(conn: &Connection) -> rusqlite::Result<u64> {
    Ok(conn.query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))? as u64)
}

/// Следующий номер своего события и следующие часы Лэмпорта.
pub fn next(conn: &Connection, device: &str) -> rusqlite::Result<(i64, i64)> {
    let seq: i64 = conn.query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE device = ?1", [device], |r| r.get(0))?;
    let lamport: i64 = conn.query_row("SELECT COALESCE(MAX(lamport), 0) + 1 FROM events", [], |r| r.get(0))?;
    Ok((seq, lamport))
}

/// Записывает событие в журнал; `false` — такое (устройство и номер) уже есть, ничего не записано.
pub fn insert_event(conn: &Connection, device: &str, seq: i64, lamport: i64, body: &str) -> rusqlite::Result<bool> {
    Ok(conn.execute("INSERT OR IGNORE INTO events(device, seq, lamport, body) VALUES(?1, ?2, ?3, ?4)", params![device, seq, lamport, body])? > 0)
}

/// Последний номер события каждого устройства — что эта база уже знает.
pub fn versions(conn: &Connection) -> rusqlite::Result<BTreeMap<String, i64>> {
    let mut st = conn.prepare("SELECT device, MAX(seq) FROM events GROUP BY device")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    rows.collect()
}

/// События одного устройства после номера `after`, по порядку.
pub fn events_after(conn: &Connection, device: &str, after: i64) -> rusqlite::Result<Vec<Row>> {
    let mut st = conn.prepare("SELECT device, seq, lamport, body FROM events WHERE device = ?1 AND seq > ?2 ORDER BY seq")?;
    let rows = st.query_map(params![device, after], row)?;
    rows.collect()
}

/// События одного устройства с номерами from..=to, по порядку.
pub fn events_range(conn: &Connection, device: &str, from: i64, to: i64) -> rusqlite::Result<Vec<Row>> {
    let mut st = conn.prepare("SELECT device, seq, lamport, body FROM events WHERE device = ?1 AND seq BETWEEN ?2 AND ?3 ORDER BY seq")?;
    let rows = st.query_map(params![device, from, to], row)?;
    rows.collect()
}

/// Все события в порядке часов Лэмпорта (при равенстве — устройство, номер).
pub fn events_by_lamport(conn: &Connection) -> rusqlite::Result<Vec<Row>> {
    let mut st = conn.prepare("SELECT device, seq, lamport, body FROM events ORDER BY lamport, device, seq")?;
    let rows = st.query_map([], row)?;
    rows.collect()
}

/// Тело события, если оно есть в журнале.
pub fn event_body(conn: &Connection, device: &str, seq: i64) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT body FROM events WHERE device = ?1 AND seq = ?2", params![device, seq], |r| r.get(0)).optional()
}

/// Сохраняет вложение под номером `id` (проверки — у приложения); повтор того же номера ничего не меняет.
/// `added` — когда добавлено (секунды Unix).
pub fn insert_blob(conn: &Connection, id: &str, mime: &str, data: &[u8], added: i64) -> rusqlite::Result<()> {
    conn.execute("INSERT OR IGNORE INTO blobs(id, mime, size, data, added) VALUES(?1, ?2, ?3, ?4, ?5)", params![id, mime, data.len() as i64, data, added])?;
    Ok(())
}

/// Вложение: тип и содержимое.
pub fn blob(conn: &Connection, id: &str) -> rusqlite::Result<Option<(String, Vec<u8>)>> {
    conn.query_row("SELECT mime, data FROM blobs WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()
}

pub fn has_blob(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    Ok(conn.query_row("SELECT 1 FROM blobs WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some())
}

/// Номера всех вложений в базе, по времени добавления.
pub fn blob_ids(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut st = conn.prepare("SELECT id FROM blobs ORDER BY added, id")?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

#[cfg(test)]
mod tests;
