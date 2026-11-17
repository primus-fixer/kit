//! Протокол синхронизации двух устройств: журналы событий, вложения и большие файлы кусками с докачкой.
//!
//! Обмен идёт по любому каналу ([`Wire`]): шифрованному каналу Wi-Fi или старому каналу по ключу из QR-кода.
//! Сообщения — JSON. Порядок:
//! 1. подключившийся → `hello` (кто я, версии журнала); ждущий → `batch` (его события, которых нет у первого);
//! 2. подключившийся принимает их и → `batch` (свои недостающие события); ждущий → `done` (сколько принял);
//! 3. круги вложений: `want` (каких не хватает) ↔ `give` (вложения и свой список) → `done`;
//! 4. большие файлы, если обе стороны умеют: `files`, затем `pull` ↔ `part` по кускам, `files_done`;
//! 5. `done` — конец.
//!
//! Так «Карман» и «Полка» обменивались и до общего модуля: сообщения те же байт в байт. Тело события
//! протоколу не нужно — его отдаёт и принимает журнал приложения ([`Journal`]).

use crate::error::{Error, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::net::TcpStream;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// Версии журнала: последний номер события каждого устройства.
pub type VersionVector = BTreeMap<String, u64>;

/// Предел сообщения.
pub const MAX_FRAME: usize = 64 * 1024 * 1024;
/// Предел приветствия: до проверки ключа большие сообщения не принимаем.
pub const HELLO_MAX: usize = 1024 * 1024;
const HELLO_WAIT: Duration = Duration::from_secs(15);
/// Столько байт вложений в одном пакете — с запасом до предела кадра.
const FILES_PER_PACKET: usize = 24 * 1024 * 1024;
/// Больше кругов обмена вложениями за одно подключение не делаем.
const MAX_ROUNDS: usize = 50;
/// Больших файлов за одно подключение.
const MAX_FILES: usize = 100;
/// Кусков за одно подключение.
const MAX_PARTS: usize = MAX_FILES * 14 + 16;

/// Большой файл, который передаётся кусками: номер (отпечаток) и размер.
pub trait FileInfo: Clone {
    fn id(&self) -> &str;
    fn size(&self) -> u64;
}

/// Больших файлов у приложения нет.
#[derive(Clone, Debug)]
pub enum NoFile {}

impl FileInfo for NoFile {
    fn id(&self) -> &str {
        match *self {}
    }
    fn size(&self) -> u64 {
        match *self {}
    }
}

/// Журнал приложения: что у него есть и как принять чужое. Реализует база приложения.
pub trait Journal {
    /// Событие журнала — протокол передаёт его как есть.
    type Event: Serialize + DeserializeOwned;
    /// Большой файл, если приложение передаёт файлы кусками (иначе [`NoFile`]).
    type File: FileInfo;

    /// Номер этого устройства.
    fn device(&self) -> String;
    fn versions(&self) -> Result<VersionVector>;
    /// События, которых нет у другой стороны.
    fn events_missing(&self, theirs: &VersionVector) -> Result<Vec<Self::Event>>;
    /// Принять чужие события; сколько новых.
    fn apply_remote(&mut self, events: &[Self::Event]) -> Result<usize>;

    /// Вложения, на которые ссылаются записи, но которых нет на устройстве.
    fn missing_blobs(&self) -> Result<Vec<String>>;
    fn blob(&self, id: &str) -> Result<Option<(String, Vec<u8>)>>;
    /// Сохранить вложение, если его отпечаток совпал с номером.
    fn put_blob_checked(&mut self, id: &str, mime: &str, data: &[u8]) -> Result<()>;

    /// Обмен большими файлами включён на этом устройстве.
    fn files_on(&self) -> bool {
        false
    }
    /// Файлы, которых не хватает.
    fn files_wanted(&self) -> Result<Vec<Self::File>> {
        Ok(Vec::new())
    }
    /// Файлы, которые есть и которые можно отдать.
    fn files_present(&self) -> Result<Vec<Self::File>> {
        Ok(Vec::new())
    }
    /// Сколько байт файла уже докачано.
    fn file_partial(&self, _id: &str) -> u64 {
        0
    }
    /// Кусок файла с места `offset`: размер файла и данные.
    fn file_read(&self, _id: &str, _offset: u64) -> Result<Option<(u64, Vec<u8>)>> {
        Ok(None)
    }
    /// Записать кусок; `true` — файл собран целиком и сошёлся с отпечатком.
    fn file_write(&mut self, _file: &Self::File, _offset: u64, _total: u64, _data: &[u8]) -> Result<bool> {
        Ok(false)
    }
}

/// Чем обмен приложения отличается.
#[derive(Clone, Copy, Debug)]
pub struct Profile {
    /// Версия приложения — уходит другой стороне.
    pub version: &'static str,
    /// «Карман»: в приветствии и пакете — поле `files` (умею вложения). Версии до 0.4 его не присылают,
    /// и шаг с вложениями с ними пропускается. У приложения без старых версий — `false`: поля нет,
    /// вложения есть всегда.
    pub files_flag: bool,
    /// Что сказать, если старая версия (без `files`) оборвала связь на новых записях.
    pub old_peer: &'static str,
    /// С какой версии другая сторона передаёт большие файлы кусками: `(0, 8)`; `None` — не передаёт.
    pub chunked_since: Option<(u32, u32)>,
}

/// Сообщения обмена.
#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", bound = "E: Serialize + DeserializeOwned")]
pub enum Msg<E> {
    /// Кто я — сразу после связи по коду.
    Ident { device: String, name: String, app: String },
    Hello {
        device: String,
        name: String,
        vv: VersionVector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        files: Option<bool>,
        #[serde(default)]
        app: String,
    },
    Batch {
        device: String,
        name: String,
        vv: VersionVector,
        events: Vec<E>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        files: Option<bool>,
        #[serde(default)]
        app: String,
    },
    Done { applied: usize },
    /// Каких вложений не хватает.
    Want { ids: Vec<String> },
    /// Вложения по запросу и свой список недостающих.
    Give { blobs: Vec<Blob>, want: Vec<String> },
    /// Большие файлы: готово ли устройство отдавать свои.
    Files { give: bool },
    /// Кусок файла с места `offset`.
    Pull { id: String, offset: u64 },
    /// Кусок файла: `total` — размер файла (0 — такого файла нет), данные — base64.
    Part { id: String, offset: u64, total: u64, data: String },
    /// Больше кусков не нужно.
    FilesDone,
}

#[derive(Serialize, Deserialize)]
pub struct Blob {
    id: String,
    mime: String,
    /// Содержимое в base64.
    data: String,
}

/// Чем закончился обмен.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct SyncReport {
    pub peer: String,
    pub peer_name: String,
    /// Сколько событий ушло другому устройству и было им принято.
    pub sent: usize,
    /// Сколько новых событий пришло к нам.
    pub received: usize,
    /// Сколько вложений ушло и пришло.
    pub files_sent: usize,
    pub files_received: usize,
    /// Версия приложения на другом устройстве.
    pub peer_app: String,
    /// Сколько больших файлов ушло и пришло целиком.
    pub whole_sent: usize,
    pub whole_received: usize,
}

/// Канал обмена: сообщения (байты JSON) поверх TCP, обычно зашифрованные.
pub trait Wire {
    fn send_bytes(&mut self, data: &[u8]) -> Result<()>;
    /// Следующее сообщение, не больше `max` байт.
    fn recv_bytes(&mut self, max: usize) -> Result<Vec<u8>>;
    fn stream(&self) -> &TcpStream;
}

/// Отправить сообщение: JSON, как его отправляли приложения до общего модуля.
pub fn send<E: Serialize + DeserializeOwned>(w: &mut impl Wire, msg: &Msg<E>) -> Result<()> {
    w.send_bytes(&serde_json::to_vec(msg)?)
}

/// Следующее сообщение, не больше `max` байт.
pub fn recv_max<E: Serialize + DeserializeOwned>(w: &mut impl Wire, max: usize) -> Result<Msg<E>> {
    Ok(serde_json::from_slice(&w.recv_bytes(max)?)?)
}

fn recv<E: Serialize + DeserializeOwned>(w: &mut impl Wire) -> Result<Msg<E>> {
    recv_max(w, MAX_FRAME)
}

fn lock<J>(store: &Mutex<J>) -> MutexGuard<'_, J> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

/// Сроки ожидания: пакет с вложениями по медленному Wi-Fi идёт долго.
pub fn prepare(s: &TcpStream) -> Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(60)))?;
    s.set_write_timeout(Some(Duration::from_secs(60)))?;
    s.set_nodelay(true)?;
    Ok(())
}

fn unexpected() -> Error {
    Error::Format("неожиданный ответ другого устройства".into())
}

/// Умеет ли другая сторона большие файлы (по её версии).
fn speaks_files(p: &Profile, app: &str) -> bool {
    let Some((maj, min)) = p.chunked_since else { return false };
    let mut it = app.split('.').map(|x| x.trim().parse::<u32>().unwrap_or(0));
    let v = (it.next().unwrap_or(0), it.next().unwrap_or(0));
    v >= (maj, min)
}

/// Умеет ли другая сторона вложения: поле `files` или, без него, — свойство приложения.
fn peer_files(p: &Profile, files: Option<bool>) -> bool {
    files.unwrap_or(!p.files_flag)
}

fn files_field(p: &Profile) -> Option<bool> {
    p.files_flag.then_some(true)
}

/// Достаёт запрошенные вложения, пока не наберётся предел пакета. Возвращает вложения и сколько из
/// имеющихся не поместилось.
fn collect_blobs<J: Journal>(store: &Mutex<J>, ids: &[String]) -> Result<(Vec<Blob>, usize)> {
    let st = lock(store);
    let (mut out, mut total, mut left) = (Vec::new(), 0usize, 0usize);
    for id in ids {
        if let Some((mime, data)) = st.blob(id)? {
            if total + data.len() > FILES_PER_PACKET && !out.is_empty() {
                left += 1;
                continue;
            }
            total += data.len();
            out.push(Blob { id: id.clone(), mime, data: B64.encode(&data) });
        }
    }
    Ok((out, left))
}

/// Сохраняет полученные вложения: только запрошенные и только с верным отпечатком.
fn store_blobs<J: Journal>(store: &Mutex<J>, asked: &[String], blobs: Vec<Blob>) -> usize {
    let mut st = lock(store);
    let mut n = 0;
    for b in blobs {
        if !asked.contains(&b.id) {
            continue;
        }
        let Ok(data) = B64.decode(b.data.as_bytes()) else { continue };
        if st.put_blob_checked(&b.id, &b.mime, &data).is_ok() {
            n += 1;
        }
    }
    n
}

/// Забрать у другой стороны недостающие файлы — кусками, с докачкой. Возвращает, сколько пришло целиком.
fn pull_files<J: Journal>(w: &mut impl Wire, store: &Mutex<J>, list: &[J::File]) -> Result<usize> {
    let mut got = 0;
    for f in list.iter().take(MAX_FILES) {
        let mut offset = lock(store).file_partial(f.id());
        if offset >= f.size() {
            offset = 0;
        }
        loop {
            send::<J::Event>(w, &Msg::Pull { id: f.id().to_string(), offset })?;
            let (id, at, total, data) = match recv::<J::Event>(w)? {
                Msg::Part { id, offset, total, data } => (id, offset, total, data),
                _ => return Err(unexpected()),
            };
            if id != f.id() || at != offset || total == 0 {
                break;
            }
            let Ok(bytes) = B64.decode(data.as_bytes()) else { break };
            match lock(store).file_write(f, offset, total, &bytes) {
                Ok(true) => {
                    got += 1;
                    break;
                }
                Ok(false) => offset += bytes.len() as u64,
                // Испорченный или не тот файл — в другой раз.
                Err(_) => break,
            }
        }
    }
    Ok(got)
}

/// Отдавать куски по запросам другой стороны, пока она не скажет «хватит». Возвращает, сколько файлов
/// отдано целиком.
fn serve_files<J: Journal>(w: &mut impl Wire, store: &Mutex<J>, give: bool) -> Result<usize> {
    let known: BTreeSet<String> = if give { lock(store).files_present()?.iter().map(|f| f.id().to_string()).collect() } else { BTreeSet::new() };
    let mut sent = 0;
    for _ in 0..MAX_PARTS {
        match recv::<J::Event>(w)? {
            Msg::Pull { id, offset } => {
                let part = if known.contains(&id) { lock(store).file_read(&id, offset)? } else { None };
                match part {
                    Some((total, data)) if !data.is_empty() => {
                        if offset + data.len() as u64 >= total {
                            sent += 1;
                        }
                        send::<J::Event>(w, &Msg::Part { id, offset, total, data: B64.encode(&data) })?;
                    }
                    _ => send::<J::Event>(w, &Msg::Part { id, offset, total: 0, data: String::new() })?,
                }
            }
            Msg::FilesDone => return Ok(sent),
            _ => return Err(unexpected()),
        }
    }
    Err(unexpected())
}

/// Обмен со стороны подключившегося. `expect` — какое устройство должно ответить (по ключу пары): ответ
/// другого — обрыв до того, как его записи приняты.
pub fn run_client<J: Journal>(p: &Profile, w: &mut impl Wire, store: &Mutex<J>, name: &str, expect: Option<&str>) -> Result<SyncReport> {
    prepare(w.stream())?;
    let (device, vv) = {
        let st = lock(store);
        (st.device(), st.versions()?)
    };
    send::<J::Event>(w, &Msg::Hello { device: device.clone(), name: name.into(), vv, files: files_field(p), app: p.version.into() })?;
    let (peer, peer_name, their_vv, events, their_files, peer_app) = match recv::<J::Event>(w)? {
        Msg::Batch { device, name, vv, events, files, app } => (device, name, vv, events, files, app),
        _ => return Err(unexpected()),
    };
    if expect.is_some_and(|e| e != peer) {
        return Err(Error::Format("ответило не то устройство".into()));
    }
    let blobs = peer_files(p, their_files);
    let (received, ours) = {
        let mut st = lock(store);
        let n = st.apply_remote(&events)?;
        (n, st.events_missing(&their_vv)?)
    };
    let vv = lock(store).versions()?;
    send::<J::Event>(w, &Msg::Batch { device, name: name.into(), vv, events: ours, files: files_field(p), app: p.version.into() })?;
    let sent = match recv::<J::Event>(w) {
        Ok(Msg::Done { applied }) => applied,
        // Старая версия не разобрала пакет с записями новых видов и оборвала связь.
        Err(_) if !blobs => return Err(Error::Peer(p.old_peer.into())),
        Err(e) => return Err(e),
        _ => return Err(unexpected()),
    };
    let mut report = SyncReport { peer, peer_name, sent, received, peer_app, ..Default::default() };
    if !blobs {
        return Ok(report);
    }
    for _ in 0..MAX_ROUNDS {
        let want = lock(store).missing_blobs()?;
        send::<J::Event>(w, &Msg::Want { ids: want.clone() })?;
        let (incoming, their_want) = match recv::<J::Event>(w)? {
            Msg::Give { blobs, want } => (blobs, want),
            _ => return Err(unexpected()),
        };
        let got = store_blobs(store, &want, incoming);
        report.files_received += got;
        let (mine, left) = collect_blobs(store, &their_want)?;
        send::<J::Event>(w, &Msg::Give { blobs: mine, want: Vec::new() })?;
        match recv::<J::Event>(w)? {
            Msg::Done { applied } => report.files_sent += applied,
            _ => return Err(unexpected()),
        }
        let still = lock(store).missing_blobs()?.len();
        if (still == 0 || got == 0) && left == 0 {
            break;
        }
    }
    // Большие файлы: сначала забираем свои недостающие, потом отдаём то, что нужно другой стороне.
    if speaks_files(p, &report.peer_app) && lock(store).files_on() {
        let mine = lock(store).files_wanted()?;
        send::<J::Event>(w, &Msg::Files { give: true })?;
        let they_give = match recv::<J::Event>(w)? {
            Msg::Files { give } => give,
            _ => return Err(unexpected()),
        };
        if they_give {
            report.whole_received = pull_files(w, store, &mine)?;
        }
        send::<J::Event>(w, &Msg::FilesDone)?;
        report.whole_sent = serve_files(w, store, true)?;
    }
    send::<J::Event>(w, &Msg::Done { applied: 0 })?;
    Ok(report)
}

/// Обмен со стороны ждущего. `expect` — какое устройство должно прийти (по ключу пары): другое имя
/// в приветствии — обрыв.
pub fn run_server<J: Journal>(p: &Profile, w: &mut impl Wire, store: &Mutex<J>, name: &str, expect: Option<&str>) -> Result<SyncReport> {
    prepare(w.stream())?;
    w.stream().set_read_timeout(Some(HELLO_WAIT))?;
    let (peer, peer_name, their_vv, their_files, peer_app) = match recv_max::<J::Event>(w, HELLO_MAX)? {
        Msg::Hello { device, name, vv, files, app } => (device, name, vv, files, app),
        _ => return Err(Error::Format("ожидалось приветствие".into())),
    };
    if expect.is_some_and(|e| e != peer) {
        return Err(Error::Format("приветствие не от того устройства".into()));
    }
    let blobs = peer_files(p, their_files);
    // Ключ подошёл — дальше пакеты с вложениями, их можно ждать дольше.
    prepare(w.stream())?;
    let (device, vv, events) = {
        let st = lock(store);
        (st.device(), st.versions()?, st.events_missing(&their_vv)?)
    };
    let sent = events.len();
    send::<J::Event>(w, &Msg::Batch { device, name: name.into(), vv, events, files: files_field(p), app: p.version.into() })?;
    let incoming = match recv::<J::Event>(w) {
        Ok(Msg::Batch { events, .. }) => events,
        Err(_) if !blobs => return Err(Error::Peer(p.old_peer.into())),
        Err(e) => return Err(e),
        _ => return Err(Error::Format("ожидались события".into())),
    };
    let received = lock(store).apply_remote(&incoming)?;
    send::<J::Event>(w, &Msg::Done { applied: received })?;
    let mut report = SyncReport { peer, peer_name, sent, received, peer_app, ..Default::default() };
    if !blobs {
        return Ok(report);
    }
    for _ in 0..MAX_ROUNDS {
        match recv::<J::Event>(w)? {
            Msg::Files { give: they_give } if p.chunked_since.is_some() => {
                let on = lock(store).files_on();
                let mine = if on { lock(store).files_wanted()? } else { Vec::new() };
                send::<J::Event>(w, &Msg::Files { give: on })?;
                report.whole_sent += serve_files(w, store, on)?;
                if on && they_give {
                    report.whole_received += pull_files(w, store, &mine)?;
                }
                send::<J::Event>(w, &Msg::FilesDone)?;
            }
            Msg::Want { ids } => {
                let (mine, _) = collect_blobs(store, &ids)?;
                report.files_sent += mine.len();
                let want = lock(store).missing_blobs()?;
                send::<J::Event>(w, &Msg::Give { blobs: mine, want: want.clone() })?;
                let incoming = match recv::<J::Event>(w)? {
                    Msg::Give { blobs, .. } => blobs,
                    _ => return Err(Error::Format("ожидались вложения".into())),
                };
                let n = store_blobs(store, &want, incoming);
                report.files_received += n;
                send::<J::Event>(w, &Msg::Done { applied: n })?;
            }
            Msg::Done { .. } => break,
            _ => return Err(Error::Format("неожиданное сообщение".into())),
        }
    }
    Ok(report)
}

#[cfg(test)]
pub(crate) mod tests;
