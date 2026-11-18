//! Облако — общее, из kit (`kit_core::cloud`): ключ облака из пароля копий, шифрование и имена файлов,
//! обмен событиями и снимками. Здесь — то, чем «Заготовка» отличается: файл ключа, метка файлов, расширения.
//!
//! **У выпущенного приложения не менять:** устройства перестанут читать то, что уже лежит в облаке.

use crate::db::{self, Event, Store};
use crate::error::store_err;
use kit_core::cloud::{self as kit, CloudStore};

/// Предел файла снимка в облаке: снимок и немного на заголовок.
pub const MAX_BLOB_FILE: usize = db::MAX_BLOB + 1024;

fn image_ok(mime: &str, data: &[u8]) -> bool {
    db::check_blob(mime, data).is_ok()
}

pub static FORMAT: kit::Format = kit::Format {
    names: crate::NAMES,
    key_file: "starter.key",
    magic: *b"ZGTC",
    events_ext: ".zev",
    // Номера событий в базе — i64.
    max_seq: i64::MAX as u64,
    blobs: kit::Blobs { ext: ".zbl", max_file: MAX_BLOB_FILE, what: "снимок", this: "этот снимок", of: "снимка", feminine: false, check: image_ok },
    files: None,
};

/// Чего обмену с облаком нужно от базы сверх обмена по Wi-Fi.
impl CloudStore for Store {
    fn origin(e: &Event) -> (&str, u64) {
        (&e.device, e.seq.max(0) as u64)
    }
    fn events_range(&self, author: &str, from: u64, to: u64) -> kit_core::Result<Vec<Event>> {
        Store::events_range(self, author, from as i64, to as i64).map_err(store_err)
    }
    fn differs(&self, e: &Event) -> kit_core::Result<bool> {
        Ok(Store::event_body(self, &e.device, e.seq).map_err(store_err)?.is_some_and(|b| b != e.body))
    }
    fn present_blobs(&self) -> kit_core::Result<Vec<String>> {
        Store::present_blobs(self).map_err(store_err)
    }
}
