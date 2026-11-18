//! Проверки ядра: записи, спор двух устройств и обмен через облако (в памяти).
//! Оставьте их в приложении и допишите свои — для своих записей.

use crate::cloud::FORMAT;
use crate::db::{Store, MAX_BLOB};
use crate::model::{Note, NOTE};
use kit_core::backup::Kdf;
use kit_core::cloud::{join, sync, MemCloud, Round, State};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

fn store(dir: &tempfile::TempDir, device: &str) -> Store {
    Store::open(&dir.path().join(format!("{device}.db")), &[9; 32], device).unwrap()
}

fn note(id: &str, text: &str) -> Note {
    Note { id: id.into(), text: text.into(), created: 1, image: None }
}

fn png() -> Vec<u8> {
    let mut d = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10];
    d.extend_from_slice(&[7; 64]);
    d
}

#[test]
fn records_live_in_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(&dir, "a");
    s.put(NOTE, "1", &note("1", "первая")).unwrap();
    s.put(NOTE, "2", &note("2", "вторая")).unwrap();
    s.put(NOTE, "1", &note("1", "первая, исправлена")).unwrap();
    s.delete(NOTE, "2").unwrap();
    let all: Vec<Note> = s.list(NOTE).unwrap();
    assert_eq!(all, [note("1", "первая, исправлена")]);
    assert_eq!(s.get::<Note>(NOTE, "2").unwrap(), None);
    assert_eq!(s.event_count().unwrap(), 4);
    // Ключ не тот — база не открывается.
    drop(s);
    assert!(matches!(Store::open(&dir.path().join("a.db"), &[1; 32], "a"), Err(crate::Error::WrongKey)));
}

#[test]
fn later_change_wins_on_both_devices() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = store(&dir, "a");
    let mut b = store(&dir, "b");
    a.put(NOTE, "1", &note("1", "с телефона")).unwrap();
    b.apply_remote(&a.events_missing(&Default::default()).unwrap()).unwrap();
    b.put(NOTE, "1", &note("1", "с компьютера")).unwrap();
    a.put(NOTE, "1", &note("1", "снова с телефона")).unwrap();
    // Обменялись в обе стороны: на обоих устройствах одно и то же.
    let to_a = b.events_missing(&a.versions().unwrap()).unwrap();
    let to_b = a.events_missing(&b.versions().unwrap()).unwrap();
    a.apply_remote(&to_a).unwrap();
    b.apply_remote(&to_b).unwrap();
    assert_eq!(a.list::<Note>(NOTE).unwrap(), b.list::<Note>(NOTE).unwrap());
    // Повтор ничего не меняет.
    assert_eq!(a.apply_remote(&to_a).unwrap(), 0);
}

#[test]
fn images_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(&dir, "a");
    let id = s.put_blob("image/png", &png()).unwrap();
    assert!(s.has_blob(&id).unwrap());
    assert!(s.put_blob("image/jpeg", &png()).is_err());
    assert!(s.put_blob("image/png", &vec![0x89; MAX_BLOB + 1]).is_err());
    assert!(s.put_blob_checked("0000", "image/png", &png()).is_err());
    s.put(NOTE, "1", &Note { image: Some("ff".repeat(32)), ..note("1", "со снимком") }).unwrap();
    assert_eq!(s.missing_blobs().unwrap(), ["ff".repeat(32)]);
}

#[test]
fn two_devices_exchange_through_the_cloud() {
    let dir = tempfile::tempdir().unwrap();
    let a = Mutex::new(store(&dir, "a"));
    let b = Mutex::new(store(&dir, "b"));
    let image = {
        let mut s = a.lock().unwrap();
        let id = s.put_blob("image/png", &png()).unwrap();
        s.put(NOTE, "1", &Note { image: Some(id.clone()), ..note("1", "из облака") }).unwrap();
        id
    };
    let cloud = MemCloud::default();
    let fast = Kdf { m_kib: 8 * 1024, t: 1, p: 1 };
    let (ka, created) = join(&FORMAT, &cloud, "пароль копий", fast, 1).unwrap();
    assert!(created);
    let (kb, created) = join(&FORMAT, &cloud, "пароль копий", fast, 2).unwrap();
    assert!(!created);
    let cancel = AtomicBool::new(false);
    let round = |store: &Mutex<Store>, key| sync(&Round { cloud: &cloud, store, key, now: 1_794_600_000, cancel: &cancel }, &mut State::default(), &mut |_| {}).unwrap();
    assert_eq!(round(&a, &ka).sent, 1);
    let got = round(&b, &kb);
    assert_eq!((got.received, got.files_received), (1, 1));
    let b = b.lock().unwrap();
    assert_eq!(b.list::<Note>(NOTE).unwrap()[0].text, "из облака");
    assert!(b.has_blob(&image).unwrap());
}
