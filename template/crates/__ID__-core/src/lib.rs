//! Ядро «__NAME_GEN__»: записи и чем «__NAME__» отличается от других приложений на общем модуле kit.
//!
//! Хранилище записей (журнал «вид, номер, содержимое»), обмен по Wi-Fi, облако и формат копий — общие,
//! из kit (`kit_core::store`, `lan`, `cloud`, `backup`). Здесь — название, порты, метки и имена файлов.
//!
//! **Строки ниже у выпущенного приложения не меняются:** с другими метками и портами устройства перестанут
//! узнавать друг друга, облако и резервные копии — открываться. Проверка `strings_are_kept` падает, если их тронуть.

use std::path::Path;

pub use kit_core::store::{Event, EventBody, Store};

/// Как «__NAME__» называется в сообщениях.
pub const NAMES: kit_core::AppNames = kit_core::AppNames { id: "__ID__", name: "__NAME__", name_acc: "__NAME_ACC__", name_gen: "__NAME_GEN__", feminine: __FEMININE__ };

/// Версия — уходит другим устройствам при обмене.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Вложения записей. Пока их нет; появятся — их тип, предел и поля со ссылками задаются здесь.
pub static BLOBS: kit_core::store::Blobs = kit_core::store::NO_BLOBS;

/// Обмен записями между устройствами: версия; больших файлов кусками нет.
pub const PROFILE: kit_core::sync::Profile = kit_core::sync::Profile { version: APP_VERSION, files_flag: false, old_peer: "", chunked_since: None };

/// Связь по Wi-Fi: свои порты и метки, чтобы приложения на kit в одной сети не мешали друг другу.
pub const LAN: kit_core::lan::Lan = kit_core::lan::Lan {
    names: NAMES,
    profile: &PROFILE,
    tcp_port: __TCP_PORT__,
    discovery_port: __UDP_PORT__,
    magic: *b"__TAG__L",
    query_magic: *b"__TAG__Q",
    answer_magic: *b"__TAG__A",
    joiner: b"__ID__-joiner",
    host: b"__ID__-host",
    personal: b"__ID__-lan",
    not_ours: "это не «__NAME__»",
    qr_scheme: "__ID__://pair?",
    qr_version: 1,
};

fn no_blob(_mime: &str, _data: &[u8]) -> bool {
    false
}

/// Облако: файл ключа, метка файлов и расширения.
pub static CLOUD: kit_core::cloud::Format = kit_core::cloud::Format {
    names: NAMES,
    key_file: "__ID__.key",
    magic: *b"__TAG__C",
    events_ext: ".__E__ev",
    // Номера событий в базе — i64.
    max_seq: i64::MAX as u64,
    blobs: kit_core::cloud::Blobs { ext: ".__E__bl", max_file: 1024, what: "вложение", this: "это вложение", of: "вложения", feminine: false, check: no_blob },
    files: None,
};

/// Резервная копия: метка в начале файла, расширение и слова-пароли, которые перебирают первыми.
/// Пароль копий нужен и облаку: из него выводится ключ облака.
pub const BACKUP: kit_core::backup::Format = kit_core::backup::Format { magic: b"__MAGIC8__", ext: "__ID__", names: NAMES, common: &["__ID__", "__NAME_LC__"], records: &[] };

/// Открывает зашифрованную базу «__NAME_GEN__». `device` — номер этого устройства.
pub fn open(path: &Path, key: &[u8; 32], device: &str) -> Result<Store, String> {
    Store::open(path, key, device, &BLOBS).map_err(|e| match e {
        kit_core::db::Open::WrongKey => "ключ не подходит к базе — восстановите файл key.sec или резервную копию".to_string(),
        kit_core::db::Open::Db(e) => format!("база не открылась: {e}"),
    })
}

/// Новый номер записи или устройства: 16 шестнадцатеричных знаков.
pub fn new_id() -> Result<String, String> {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).map_err(|e| format!("генератор случайных чисел: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Строки, которые у выпущенного приложения менять нельзя.
    #[test]
    fn strings_are_kept() {
        assert_eq!(NAMES.id, "__ID__");
        assert_eq!((LAN.tcp_port, LAN.discovery_port), (__TCP_PORT__, __UDP_PORT__));
        assert_eq!(&LAN.magic, b"__TAG__L");
        assert_eq!(LAN.personal, b"__ID__-lan");
        assert_eq!(CLOUD.key_file, "__ID__.key");
        assert_eq!(&CLOUD.magic, b"__TAG__C");
        assert_eq!(BACKUP.magic, b"__MAGIC8__");
    }

    #[test]
    fn records_live_in_the_store() {
        let dir = std::env::temp_dir().join(format!("__ID__-core-{}", new_id().unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = open(&dir.join("__ID__.db"), &[9u8; 32], "aaaa").unwrap();
        store.put("setting", "view", &"list").unwrap();
        assert_eq!(store.get::<String>("setting", "view").unwrap().as_deref(), Some("list"));
        drop(store);
        assert!(open(&dir.join("__ID__.db"), &[8u8; 32], "aaaa").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ids_are_random_hex() {
        let a = new_id().unwrap();
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, new_id().unwrap());
    }
}
