//! Проверки на имитации сервера WebDAV: вход по паролю, файлы во всех папках, ошибки и обмен целиком.

use crate::dav::{self, Dav};
use crate::mock::{Mock, DAV_PASSWORD, DAV_USER};
use kit_core::backup::Kdf;
use kit_core::cloud::{self, Blobs, Cloud, CloudFail, CloudStore, Dir, Files, Format, Round, State};
use kit_core::sync::{Journal, NoFile, VersionVector};
use kit_core::{AppNames, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

fn image(mime: &str, data: &[u8]) -> bool {
    mime == "image/jpeg" && data.starts_with(&[0xFF, 0xD8, 0xFF])
}

pub(crate) static POLKA: Format = Format {
    names: AppNames { id: "polka", name: "Полка", name_acc: "Полку", name_gen: "Полки", feminine: true },
    key_file: "polka.key",
    magic: *b"PLKC",
    events_ext: ".pev",
    max_seq: i64::MAX as u64,
    blobs: Blobs { ext: ".pbl", max_file: (8 << 20) + 1024, what: "обложка", this: "эта обложка", of: "обложки", feminine: true, check: image },
    files: Some(Files { ext: ".pbk", part: 8 << 20, what: "Файл книги" }),
};

pub(crate) static KARMAN: Format = Format {
    names: AppNames { id: "karman", name: "Карман", name_acc: "Карман", name_gen: "Кармана", feminine: false },
    key_file: "karman.key",
    magic: *b"KRMC",
    events_ext: ".kev",
    max_seq: u64::MAX,
    blobs: Blobs { ext: ".kbl", max_file: (15 << 20) + 1024, what: "снимок чека", this: "этот снимок", of: "снимка", feminine: false, check: image },
    files: None,
};

const FAST: Kdf = Kdf { m_kib: 8 * 1024, t: 1, p: 1 };

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Ev {
    device: String,
    seq: u64,
    body: String,
}

/// Журнал в памяти: события и вложения.
#[derive(Default)]
struct Mem {
    device: String,
    events: Vec<Ev>,
    blobs: BTreeMap<String, (String, Vec<u8>)>,
    refs: Vec<String>,
}

impl Journal for Mem {
    type Event = Ev;
    type File = NoFile;
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
        let before = self.events.len();
        for e in events {
            if !self.events.iter().any(|x| x.device == e.device && x.seq == e.seq) {
                self.events.push(e.clone());
            }
        }
        Ok(self.events.len() - before)
    }
    fn missing_blobs(&self) -> Result<Vec<String>> {
        Ok(self.refs.iter().filter(|r| !self.blobs.contains_key(*r)).cloned().collect())
    }
    fn blob(&self, id: &str) -> Result<Option<(String, Vec<u8>)>> {
        Ok(self.blobs.get(id).cloned())
    }
    fn put_blob_checked(&mut self, id: &str, mime: &str, data: &[u8]) -> Result<()> {
        self.blobs.insert(id.into(), (mime.into(), data.to_vec()));
        Ok(())
    }
}

impl CloudStore for Mem {
    fn origin(e: &Ev) -> (&str, u64) {
        (&e.device, e.seq)
    }
    fn events_range(&self, author: &str, from: u64, to: u64) -> Result<Vec<Ev>> {
        let mut v: Vec<Ev> = self.events.iter().filter(|e| e.device == author && (from..=to).contains(&e.seq)).cloned().collect();
        v.sort_by_key(|e| e.seq);
        Ok(v)
    }
    fn differs(&self, e: &Ev) -> Result<bool> {
        Ok(self.events.iter().any(|x| x.device == e.device && x.seq == e.seq && x.body != e.body))
    }
    fn present_blobs(&self) -> Result<Vec<String>> {
        Ok(self.blobs.keys().cloned().collect())
    }
}


fn dav(fmt: &'static Format, m: &Mock) -> Dav {
    Dav::new(fmt, &dav::server_url(&m.dav_url(), true).unwrap(), DAV_USER, DAV_PASSWORD)
}

fn round(c: &dyn Cloud, st: &Mutex<Mem>, key: &cloud::CloudKey) -> cloud::Report {
    let cancel = AtomicBool::new(false);
    cloud::sync(&Round { cloud: c, store: st, key, now: crate::now(), cancel: &cancel }, &mut State::default(), &mut |_| {}).unwrap()
}

/// Обмен целиком через настоящие запросы: компьютер и телефон.
#[test]
fn phone_and_computer_exchange_through_webdav() {
    let m = Mock::start();
    let (c1, c2) = (dav(&KARMAN, &m), dav(&KARMAN, &m));
    let mut pc = Mem { device: "aaaa".into(), ..Default::default() };
    let mut photo = vec![9u8; 80_000];
    photo[..3].copy_from_slice(&[0xFF, 0xD8, 0xFF]);
    let id = cloud::blob_id(&photo);
    pc.blobs.insert(id.clone(), ("image/jpeg".into(), photo.clone()));
    for (i, t) in ["Хлеб", "Кофе"].into_iter().enumerate() {
        pc.events.push(Ev { device: "aaaa".into(), seq: i as u64 + 1, body: t.into() });
    }
    let pc = Mutex::new(pc);
    let phone = Mutex::new(Mem { device: "bbbb".into(), refs: vec![id.clone()], ..Default::default() });
    let (k1, created) = cloud::join(&KARMAN, &c1, "пароль копий", FAST, crate::now()).unwrap();
    assert!(created);
    let (k2, created) = cloud::join(&KARMAN, &c2, "пароль копий", FAST, crate::now()).unwrap();
    assert!(!created);
    let r = round(&c1, &pc, &k1);
    assert_eq!((r.sent, r.files_sent), (2, 1));
    let r = round(&c2, &phone, &k2);
    assert_eq!((r.received, r.files_received), (2, 1));
    assert_eq!(phone.lock().unwrap().blobs.get(&id).map(|b| b.1.clone()), Some(photo));
    let files: Vec<String> = m.files().into_iter().map(|(n, _)| n).collect();
    assert!(files.contains(&"dav:/Карман/karman.key".to_string()) && files.iter().any(|f| f.starts_with("dav:/Карман/e/aaaa-1-2.kev")), "{files:?}");
}

#[test]
fn webdav_sign_in_with_password() {
    let m = Mock::start();
    let url = dav::server_url(&m.dav_url(), true).unwrap();
    let wrong = Dav::new(&POLKA, &url, DAV_USER, "not-the-password");
    assert!(matches!(wrong.connect(), Err(Error::Cloud(CloudFail::Auth, msg)) if msg.contains("не принял")));
    assert!(m.dav_dirs().is_empty(), "без входа ничего не создаётся");
    let d = dav(&POLKA, &m);
    assert!(!d.connect().unwrap(), "данных «Полки» ещё нет");
    assert_eq!(m.dav_dirs(), ["/Полка"]);
    assert!(!dav(&POLKA, &m).connect().unwrap(), "папка уже есть — не ошибка");
    d.put(Dir::Root, POLKA.key_file, b"{}").unwrap();
    assert!(dav(&POLKA, &m).connect().unwrap(), "в облаке уже есть данные: пароль копий вводят, а не придумывают");
    assert!(!dav(&KARMAN, &m).connect().unwrap(), "у «Кармана» своя папка");
    let nowhere = Dav::new(&POLKA, &format!("{}/nowhere", m.base), DAV_USER, DAV_PASSWORD);
    assert!(matches!(nowhere.connect(), Err(Error::Cloud(CloudFail::Auth, msg)) if msg.contains("адрес")));
    m.dav_forbid(true);
    assert!(matches!(dav(&POLKA, &m).connect(), Err(Error::Cloud(CloudFail::Auth, msg)) if msg.contains("403")));
}

#[test]
fn webdav_files_in_every_folder() {
    let m = Mock::start();
    let s = dav(&POLKA, &m);
    assert!(s.list(Dir::Events).unwrap().is_empty(), "папки ещё нет — список пустой");
    assert_eq!(s.get(Dir::Root, POLKA.key_file, 1024).unwrap(), None);
    assert!(!s.exists(Dir::Root, POLKA.key_file).unwrap());
    s.put(Dir::Root, POLKA.key_file, b"{\"k\":1}").unwrap();
    s.put(Dir::Events, "aaaa-1-3.pev", &[1, 2, 3]).unwrap();
    let big = vec![7u8; 6 << 20];
    s.put(Dir::Blobs, "b1.pbl", &big).unwrap();
    s.put(Dir::Files, "c1-0-1.pbk", b"part").unwrap();
    s.put(Dir::Events, "aaaa-1-3.pev", &[4, 5]).unwrap();
    assert!(s.exists(Dir::Root, POLKA.key_file).unwrap());
    assert!(!s.exists(Dir::Root, "e").unwrap(), "папка — не файл");
    assert_eq!(s.get(Dir::Events, "aaaa-1-3.pev", 1024).unwrap(), Some(vec![4, 5]));
    assert_eq!(s.get(Dir::Blobs, "b1.pbl", 7 << 20).unwrap().map(|d| d.len()), Some(6 << 20));
    assert!(matches!(s.get(Dir::Blobs, "b1.pbl", 1024), Err(Error::Cloud(CloudFail::Format, _))), "больше предела — не скачиваем");
    let names: Vec<String> = s.list(Dir::Events).unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["aaaa-1-3.pev"]);
    assert_eq!(s.list(Dir::Blobs).unwrap()[0].size, 6 << 20);
    assert_eq!(s.list(Dir::Root).unwrap().into_iter().map(|e| e.name).collect::<Vec<_>>(), [POLKA.key_file], "папки e, b и k — не файлы");
    s.delete(Dir::Events, "aaaa-1-3.pev").unwrap();
    s.delete(Dir::Events, "aaaa-1-3.pev").unwrap();
    assert!(s.list(Dir::Events).unwrap().is_empty());
    let files: Vec<String> = m.files().into_iter().map(|(n, _)| n).collect();
    assert_eq!(files, ["dav:/Полка/b/b1.pbl", "dav:/Полка/k/c1-0-1.pbk", "dav:/Полка/polka.key"]);
    // Папку «Полка» удалили в облаке, пока «Полка» работала: она создаётся снова.
    m.dav_remove("/Полка");
    s.put(Dir::Events, "aaaa-4-4.pev", b"again").unwrap();
    assert_eq!(s.get(Dir::Events, "aaaa-4-4.pev", 1024).unwrap().as_deref(), Some(&b"again"[..]));
    // Данные удалили: пустые папки убираются, чужие файлы остаются.
    s.put(Dir::Root, "заметки.txt", b"my notes").unwrap();
    cloud::wipe(&POLKA, &s, &AtomicBool::new(false)).unwrap();
    s.tidy().unwrap();
    assert_eq!(m.files().into_iter().map(|(n, _)| n).collect::<Vec<_>>(), ["dav:/Полка/заметки.txt"]);
    assert_eq!(m.dav_dirs(), ["/Полка"]);
    s.delete(Dir::Root, "заметки.txt").unwrap();
    s.tidy().unwrap();
    assert!(m.dav_dirs().is_empty());
}

#[test]
fn webdav_full_disk_new_password_and_no_connection() {
    let m = Mock::start();
    let s = dav(&KARMAN, &m);
    s.put(Dir::Events, "aaaa-1-1.kev", b"x").unwrap();
    m.full(true);
    assert!(matches!(s.put(Dir::Blobs, "b.kbl", b"photo"), Err(Error::Cloud(CloudFail::Space, _))));
    m.full(false);
    m.dav_password(Some("another-app-password"));
    assert!(matches!(s.list(Dir::Events), Err(Error::Cloud(CloudFail::Auth, _))), "пароль приложения удалили — войти снова");
    m.dav_password(None);
    m.dav_forbid(true);
    assert!(matches!(s.exists(Dir::Root, KARMAN.key_file), Err(Error::Cloud(CloudFail::Auth, msg)) if msg.contains("«Карман»")));
    assert!(m.requests() > 0);
    let url = dav::server_url(&m.dav_url(), true).unwrap();
    drop(m);
    assert!(matches!(Dav::new(&KARMAN, &url, DAV_USER, DAV_PASSWORD).list(Dir::Events), Err(Error::Cloud(CloudFail::Offline, _))));
}
