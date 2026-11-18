//! Хранилище записей kit открывает базу «Полки» как есть: журнал «вид, номер, содержимое» у них общий.
#![cfg(feature = "store")]

use kit_core::cloud::blob_id;
use kit_core::db::Open;
use kit_core::store::{Blobs, Store};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Обложки «Полки»: JPEG, PNG и WebP до 8 МБ; ссылка — поле `cover` записи.
fn cover_ok(mime: &str, data: &[u8]) -> bool {
    let sniffed = if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        "image/webp"
    } else {
        ""
    };
    sniffed == mime
}

static COVERS: Blobs = Blobs { max: 8 * 1024 * 1024, check: cover_ok, refs: &["$.cover"] };

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("polka-0.11.1")
}

fn key(info: &Value) -> [u8; 32] {
    let hex = info["db_key"].as_str().unwrap();
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
    }
    k
}

/// Копия образца во временной папке: сам образец не трогаем.
struct Copy(PathBuf);

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn copy_of_sample() -> (Copy, PathBuf) {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).unwrap();
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let dir = std::env::temp_dir().join(format!("kit-store-polka-{hex}"));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("polka.db");
    std::fs::copy(sample().join("store.db"), &db).unwrap();
    (Copy(dir), db)
}

#[test]
fn polka_database_opens_as_is() {
    let info: Value = serde_json::from_str(&std::fs::read_to_string(sample().join("info.json")).unwrap()).unwrap();
    let (_tmp, db) = copy_of_sample();
    let device = info["device"].as_str().unwrap();
    let store = Store::open(&db, &key(&info), device, &COVERS).unwrap_or_else(|_| panic!("база «Полки» не открылась"));
    for (kind, records) in info["records"].as_object().unwrap() {
        for (id, body) in records.as_object().unwrap() {
            let got: Value = store.get(kind, id).unwrap().unwrap_or_else(|| panic!("нет записи {kind}/{id}"));
            assert_eq!(&got, body, "{kind}/{id} читается иначе");
        }
        assert_eq!(store.list::<Value>(kind).unwrap().len(), records.as_object().unwrap().len(), "записей вида {kind} не столько, сколько в образце");
    }
    assert!(store.get::<Value>("title", "gone").unwrap().is_none(), "удалённая запись вернулась");
    assert_eq!(json!(store.versions().unwrap()), info["versions"]);
    assert_eq!(json!(store.event_count().unwrap()), info["events"]);
    for (id, size) in info["blobs"].as_object().unwrap() {
        let (_, data) = store.blob(id).unwrap().unwrap_or_else(|| panic!("нет обложки {id}"));
        assert_eq!(json!(data.len()), *size);
        assert_eq!(blob_id(&data), *id);
    }
    assert!(store.missing_blobs().unwrap().is_empty());
    // Каждое событие журнала разбирается в общее тело события.
    let all = store.events_missing(&Default::default()).unwrap();
    assert_eq!(json!(all.len()), info["events"]);
    drop(store);
    assert!(matches!(Store::open(&db, &[0x16; 32], device, &COVERS), Err(Open::WrongKey)), "с чужим ключом база не открывается");
}
