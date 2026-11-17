use super::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

/// Событие для проверок: устройство, номер и содержимое.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Ev {
    pub(crate) device: String,
    pub(crate) seq: u64,
    pub(crate) body: Value,
}

#[derive(Clone, Debug)]
pub(crate) struct F {
    pub(crate) id: String,
    pub(crate) size: u64,
}

impl FileInfo for F {
    fn id(&self) -> &str {
        &self.id
    }
    fn size(&self) -> u64 {
        self.size
    }
}

/// Журнал в памяти. Вложение подходит, если его номер — «b-<длина>».
#[derive(Default)]
pub(crate) struct Mem {
    pub(crate) device: String,
    pub(crate) events: Vec<Ev>,
    pub(crate) blobs: BTreeMap<String, (String, Vec<u8>)>,
    pub(crate) refs: Vec<String>,
    pub(crate) files_on: bool,
    pub(crate) files: BTreeMap<String, Vec<u8>>,
    pub(crate) wanted: Vec<F>,
    pub(crate) partial: BTreeMap<String, Vec<u8>>,
    pub(crate) part: usize,
}

impl Mem {
    pub(crate) fn new(device: &str) -> Mem {
        Mem { device: device.into(), part: 1000, ..Default::default() }
    }
    pub(crate) fn add(&mut self, body: Value) {
        let seq = self.events.iter().filter(|e| e.device == self.device).count() as u64 + 1;
        self.events.push(Ev { device: self.device.clone(), seq, body });
    }
}

impl Journal for Mem {
    type Event = Ev;
    type File = F;

    fn device(&self) -> String {
        self.device.clone()
    }
    fn versions(&self) -> Result<VersionVector> {
        let mut vv = VersionVector::new();
        for e in &self.events {
            let v = vv.entry(e.device.clone()).or_insert(0);
            *v = (*v).max(e.seq);
        }
        Ok(vv)
    }
    fn events_missing(&self, theirs: &VersionVector) -> Result<Vec<Ev>> {
        Ok(self.events.iter().filter(|e| e.seq > theirs.get(&e.device).copied().unwrap_or(0)).cloned().collect())
    }
    fn apply_remote(&mut self, events: &[Ev]) -> Result<usize> {
        let mut n = 0;
        for e in events {
            if !self.events.iter().any(|x| x.device == e.device && x.seq == e.seq) {
                self.events.push(e.clone());
                n += 1;
            }
        }
        Ok(n)
    }
    fn missing_blobs(&self) -> Result<Vec<String>> {
        Ok(self.refs.iter().filter(|r| !self.blobs.contains_key(*r)).cloned().collect())
    }
    fn blob(&self, id: &str) -> Result<Option<(String, Vec<u8>)>> {
        Ok(self.blobs.get(id).cloned())
    }
    fn put_blob_checked(&mut self, id: &str, mime: &str, data: &[u8]) -> Result<()> {
        if id != format!("b-{}", data.len()) && id != crate::cloud::blob_id(data) {
            return Err(Error::Format("отпечаток не сошёлся".into()));
        }
        self.blobs.insert(id.into(), (mime.into(), data.to_vec()));
        Ok(())
    }
    fn files_on(&self) -> bool {
        self.files_on
    }
    fn files_wanted(&self) -> Result<Vec<F>> {
        Ok(self.wanted.iter().filter(|f| !self.files.contains_key(&f.id)).cloned().collect())
    }
    fn files_present(&self) -> Result<Vec<F>> {
        Ok(self.files.iter().map(|(id, d)| F { id: id.clone(), size: d.len() as u64 }).collect())
    }
    fn file_partial(&self, id: &str) -> u64 {
        self.partial.get(id).map(|d| d.len() as u64).unwrap_or(0)
    }
    fn file_read(&self, id: &str, offset: u64) -> Result<Option<(u64, Vec<u8>)>> {
        Ok(self.files.get(id).map(|d| {
            let start = (offset as usize).min(d.len());
            (d.len() as u64, d[start..(start + self.part).min(d.len())].to_vec())
        }))
    }
    fn file_write(&mut self, file: &F, offset: u64, total: u64, data: &[u8]) -> Result<bool> {
        let buf = self.partial.entry(file.id.clone()).or_default();
        if buf.len() as u64 != offset || total != file.size {
            return Err(Error::Format("не тот кусок".into()));
        }
        buf.extend_from_slice(data);
        if buf.len() as u64 == total {
            let whole = self.partial.remove(&file.id).unwrap();
            self.files.insert(file.id.clone(), whole);
            return Ok(true);
        }
        Ok(false)
    }
}

/// Канал без шифрования: длина и байты. Записывает все отправленные сообщения.
struct Plain {
    s: TcpStream,
    log: Arc<Mutex<Vec<String>>>,
}

impl Wire for Plain {
    fn send_bytes(&mut self, data: &[u8]) -> Result<()> {
        self.log.lock().unwrap().push(String::from_utf8(data.to_vec()).unwrap());
        self.s.write_all(&(data.len() as u32).to_be_bytes())?;
        self.s.write_all(data)?;
        Ok(())
    }
    fn recv_bytes(&mut self, max: usize) -> Result<Vec<u8>> {
        let mut len = [0u8; 4];
        self.s.read_exact(&mut len)?;
        let len = u32::from_be_bytes(len) as usize;
        if len > max {
            return Err(Error::Format("слишком большой пакет".into()));
        }
        let mut buf = vec![0u8; len];
        self.s.read_exact(&mut buf)?;
        Ok(buf)
    }
    fn stream(&self) -> &TcpStream {
        &self.s
    }
}

pub(crate) const POLKA: Profile = Profile { version: "0.11.6", files_flag: false, old_peer: "", chunked_since: Some((0, 8)) };
pub(crate) const KARMAN: Profile = Profile { version: "0.15.7", files_flag: true, old_peer: "На другом устройстве старая версия", chunked_since: None };

/// Обмен по-настоящему, через TCP: `a` подключается к `b`. Возвращает отчёты и то, что ушло по сети.
fn exchange(pa: Profile, a: Mem, pb: Profile, b: Mem) -> (Result<SyncReport>, Result<SyncReport>, Mem, Mem, Vec<String>) {
    let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let lb = log.clone();
    let server = std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let store = Mutex::new(b);
        let r = run_server(&pb, &mut Plain { s, log: lb }, &store, "Ноутбук", None);
        (r, store.into_inner().unwrap())
    });
    let store = Mutex::new(a);
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let ra = run_client(&pa, &mut Plain { s, log: log.clone() }, &store, "Телефон", None);
    let (rb, b) = server.join().unwrap();
    let log = log.lock().unwrap().clone();
    (ra, rb, store.into_inner().unwrap(), b, log)
}

#[test]
fn two_devices_exchange_events_and_blobs() {
    let mut a = Mem::new("aaaa");
    a.add(json!({ "k": "title", "v": "Лавр" }));
    a.add(json!({ "k": "cover", "v": "b-5" }));
    a.blobs.insert("b-5".into(), ("image/jpeg".into(), vec![1, 2, 3, 4, 5]));
    let mut b = Mem::new("bbbb");
    b.add(json!({ "k": "title", "v": "Солярис" }));
    b.refs.push("b-5".into());
    a.refs.push("b-3".into());
    b.blobs.insert("b-3".into(), ("image/png".into(), vec![9, 9, 9]));
    let (ra, rb, a, b, _) = exchange(POLKA, a, POLKA, b);
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    assert_eq!((ra.peer.as_str(), ra.peer_name.as_str(), ra.peer_app.as_str()), ("bbbb", "Ноутбук", "0.11.6"));
    assert_eq!((ra.sent, ra.received, ra.files_sent, ra.files_received), (2, 1, 1, 1));
    assert_eq!((rb.sent, rb.received, rb.files_sent, rb.files_received), (1, 2, 1, 1));
    assert_eq!(a.events.len(), 3);
    assert_eq!(b.events.len(), 3);
    assert!(a.blobs.contains_key("b-3") && b.blobs.contains_key("b-5"));
    // Второй обмен — ничего нового.
    let (ra, _, _, _, _) = exchange(POLKA, a, POLKA, b);
    let ra = ra.unwrap();
    assert_eq!((ra.sent, ra.received, ra.files_sent, ra.files_received), (0, 0, 0, 0));
}

/// Сообщения — те же байты, что у приложений до общего модуля: «Карман» пишет поле `files`, «Полка» — нет.
#[test]
fn messages_are_the_same_bytes_as_before() {
    let vv: VersionVector = [("aaaa".to_string(), 3)].into();
    let hello = |p: &Profile| serde_json::to_string(&Msg::<Ev>::Hello { device: "aaaa".into(), name: "Ноутбук".into(), vv: vv.clone(), files: files_field(p), app: p.version.into() }).unwrap();
    assert_eq!(hello(&KARMAN), r#"{"t":"hello","device":"aaaa","name":"Ноутбук","vv":{"aaaa":3},"files":true,"app":"0.15.7"}"#);
    assert_eq!(hello(&POLKA), r#"{"t":"hello","device":"aaaa","name":"Ноутбук","vv":{"aaaa":3},"app":"0.11.6"}"#);
    let batch = serde_json::to_string(&Msg::Batch { device: "a".into(), name: "n".into(), vv: VersionVector::new(), events: vec![Ev { device: "a".into(), seq: 1, body: json!(1) }], files: Some(true), app: "1".into() }).unwrap();
    assert_eq!(batch, r#"{"t":"batch","device":"a","name":"n","vv":{},"events":[{"device":"a","seq":1,"body":1}],"files":true,"app":"1"}"#);
    let s = |m: Msg<Ev>| serde_json::to_string(&m).unwrap();
    assert_eq!(s(Msg::Ident { device: "a".into(), name: "n".into(), app: "1".into() }), r#"{"t":"ident","device":"a","name":"n","app":"1"}"#);
    assert_eq!(s(Msg::Done { applied: 2 }), r#"{"t":"done","applied":2}"#);
    assert_eq!(s(Msg::Want { ids: vec!["x".into()] }), r#"{"t":"want","ids":["x"]}"#);
    assert_eq!(s(Msg::Give { blobs: vec![Blob { id: "x".into(), mime: "image/jpeg".into(), data: "AQ==".into() }], want: vec![] }), r#"{"t":"give","blobs":[{"id":"x","mime":"image/jpeg","data":"AQ=="}],"want":[]}"#);
    assert_eq!(s(Msg::Files { give: true }), r#"{"t":"files","give":true}"#);
    assert_eq!(s(Msg::Pull { id: "f".into(), offset: 8 }), r#"{"t":"pull","id":"f","offset":8}"#);
    assert_eq!(s(Msg::Part { id: "f".into(), offset: 8, total: 9, data: "AQ==".into() }), r#"{"t":"part","id":"f","offset":8,"total":9,"data":"AQ=="}"#);
    assert_eq!(s(Msg::FilesDone), r#"{"t":"files_done"}"#);
    // Приветствие «Кармана» старше 0.4 и старше 0.5: без `files` и без `app`.
    let old: Msg<Ev> = serde_json::from_str(r#"{"t":"hello","device":"a","name":"n","vv":{}}"#).unwrap();
    assert!(matches!(old, Msg::Hello { files: None, ref app, .. } if app.is_empty()));
}

#[test]
fn karman_peers_exchange_with_the_files_flag() {
    let mut a = Mem::new("aaaa");
    a.add(json!({ "op": 1 }));
    let mut b = Mem::new("bbbb");
    b.refs.push("b-2".into());
    a.blobs.insert("b-2".into(), ("image/jpeg".into(), vec![7, 7]));
    let (ra, rb, _, b, log) = exchange(KARMAN, a, KARMAN, b);
    assert_eq!((ra.unwrap().files_sent, rb.unwrap().received), (1, 1));
    assert!(b.blobs.contains_key("b-2"));
    assert!(log.iter().filter(|m| m.starts_with(r#"{"t":"hello""#) || m.starts_with(r#"{"t":"batch""#)).all(|m| m.contains(r#""files":true"#)));
    assert!(!log.iter().any(|m| m.contains(r#""t":"files""#)), "больших файлов у «Кармана» нет");
}

/// Старый «Карман» (до 0.4): приветствие без `files`, пакет с новыми записями он не разбирает и рвёт связь.
#[test]
fn old_karman_peer_gets_a_clear_error() {
    let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    let old = std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let mut w = Plain { s, log: Default::default() };
        let _hello: Msg<Ev> = recv(&mut w).unwrap();
        send::<Ev>(&mut w, &Msg::Batch { device: "old".into(), name: "Старый".into(), vv: VersionVector::new(), events: vec![], files: None, app: String::new() }).unwrap();
        let _ = recv::<Ev>(&mut w);
        // Обрыв без ответа.
    });
    let mut a = Mem::new("aaaa");
    a.add(json!({ "op": 1 }));
    let store = Mutex::new(a);
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let r = run_client(&KARMAN, &mut Plain { s, log: Default::default() }, &store, "Телефон", None);
    old.join().unwrap();
    assert!(matches!(r, Err(Error::Peer(m)) if m == KARMAN.old_peer));
}

/// Большие файлы — кусками, с докачкой с того места, где оборвалось.
#[test]
fn big_files_travel_in_parts_and_resume() {
    let data: Vec<u8> = (0..3500u32).map(|i| (i % 251) as u8).collect();
    let mut a = Mem::new("aaaa");
    a.files_on = true;
    a.wanted = vec![F { id: "book".into(), size: data.len() as u64 }];
    // Первая тысяча байт уже пришла в прошлый раз.
    a.partial.insert("book".into(), data[..1000].to_vec());
    let mut b = Mem::new("bbbb");
    b.files_on = true;
    b.files.insert("book".into(), data.clone());
    let (ra, rb, a, _, log) = exchange(POLKA, a, POLKA, b);
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    assert_eq!((ra.whole_received, rb.whole_sent), (1, 1));
    assert_eq!(a.files["book"], data);
    let pulls: Vec<&String> = log.iter().filter(|m| m.contains(r#""t":"pull""#)).collect();
    assert_eq!(pulls.len(), 3, "докачка: куски с 1000, 2000 и 3000 — {pulls:?}");
    assert!(pulls[0].contains(r#""offset":1000"#));
}

/// С «Полкой» старше 0.8 больших файлов не передаём; с выключенным обменом — тоже.
#[test]
fn big_files_only_when_both_can() {
    let mk = |files_on: bool| {
        let mut a = Mem::new("aaaa");
        a.files_on = files_on;
        a.wanted = vec![F { id: "book".into(), size: 10 }];
        a
    };
    let mut b = Mem::new("bbbb");
    b.files_on = true;
    b.files.insert("book".into(), vec![1; 10]);
    let old = Profile { version: "0.7.3", ..POLKA };
    let (ra, _, _, b, log) = exchange(POLKA, mk(true), old, b);
    assert_eq!(ra.unwrap().whole_received, 0);
    assert!(!log.iter().any(|m| m.contains(r#""t":"files""#)));
    let (ra, _, _, _, log) = exchange(POLKA, mk(false), POLKA, b);
    assert_eq!(ra.unwrap().whole_received, 0);
    assert!(!log.iter().any(|m| m.contains(r#""t":"files""#)));
}

/// Вложение с неверным отпечатком и незапрошенное не сохраняются.
#[test]
fn foreign_and_broken_blobs_are_dropped() {
    let mut a = Mem::new("aaaa");
    a.refs.push("b-4".into());
    let mut b = Mem::new("bbbb");
    // Под номером «b-4» — три байта: отпечаток не сойдётся.
    b.blobs.insert("b-4".into(), ("image/jpeg".into(), vec![1, 2, 3]));
    let (ra, _, a, _, _) = exchange(POLKA, a, POLKA, b);
    assert_eq!(ra.unwrap().files_received, 0);
    assert!(a.blobs.is_empty());
}

/// Ответило не то устройство (ключ пары другого): его записи не принимаются.
#[test]
fn wrong_device_is_refused() {
    let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let mut b = Mem::new("cccc");
        b.add(json!({ "чужое": true }));
        let store = Mutex::new(b);
        let _ = run_server(&POLKA, &mut Plain { s, log: Default::default() }, &store, "Чужой", None);
    });
    let store = Mutex::new(Mem::new("aaaa"));
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let r = run_client(&POLKA, &mut Plain { s, log: Default::default() }, &store, "Телефон", Some("bbbb"));
    let _ = server.join();
    assert!(matches!(r, Err(Error::Format(m)) if m.contains("не то устройство")));
    assert!(store.lock().unwrap().events.is_empty());
}
