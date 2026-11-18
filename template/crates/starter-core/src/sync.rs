//! Обмен записями между устройствами — общий протокол kit (`kit_core::sync`) по любому каналу:
//! Wi-Fi или облако. Здесь — журнал «Заготовки» для протокола и её версия.

use crate::db::{Event, Store};
use crate::error::store_err;
use kit_core::sync::{self as kit, Journal, NoFile, Profile};

/// Версия приложения: другое устройство узнаёт её при обмене.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Обмен «Заготовки»: снимки умеют все её версии, больших файлов кусками нет.
pub const PROFILE: Profile = Profile { version: APP_VERSION, files_flag: false, old_peer: "", chunked_since: None };

/// Журнал «Заготовки» для общего протокола: события и снимки.
impl Journal for Store {
    type Event = Event;
    type File = NoFile;

    fn device(&self) -> String {
        Store::device(self).to_string()
    }
    fn versions(&self) -> kit_core::Result<kit::VersionVector> {
        Ok(Store::versions(self).map_err(store_err)?.into_iter().map(|(d, s)| (d, s.max(0) as u64)).collect())
    }
    fn events_missing(&self, theirs: &kit::VersionVector) -> kit_core::Result<Vec<Event>> {
        let theirs = theirs.iter().map(|(d, s)| (d.clone(), (*s).min(i64::MAX as u64) as i64)).collect();
        Store::events_missing(self, &theirs).map_err(store_err)
    }
    fn apply_remote(&mut self, events: &[Event]) -> kit_core::Result<usize> {
        Store::apply_remote(self, events).map_err(store_err)
    }
    fn missing_blobs(&self) -> kit_core::Result<Vec<String>> {
        Store::missing_blobs(self).map_err(store_err)
    }
    fn blob(&self, id: &str) -> kit_core::Result<Option<(String, Vec<u8>)>> {
        Store::blob(self, id).map_err(store_err)
    }
    fn put_blob_checked(&mut self, id: &str, mime: &str, data: &[u8]) -> kit_core::Result<()> {
        Store::put_blob_checked(self, id, mime, data).map_err(store_err)
    }
}
