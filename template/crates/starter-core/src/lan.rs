//! Связь по Wi-Fi — общая, из kit (`kit_core::lan`): поиск в сети, связь по коду (SPAKE2), ключ пары,
//! шифрованный канал. Здесь — то, чем «Заготовка» отличается от других приложений: свои порты, метки
//! и строки ключей, чтобы приложения в одной сети не мешали друг другу.
//!
//! **У выпущенного приложения не менять:** устройства с прежней версией перестанут находить друг друга.
//! Занятые порты: «Карман» — 47500/47501, «Полка» — 47510/47511.

use crate::db::Store;
use crate::sync::PROFILE;
use kit_core::lan as kit;

/// Начало QR-кода связи: `starter://pair?…`.
pub const SCHEME: &str = "starter://pair?";

pub const LAN: kit::Lan = kit::Lan {
    names: crate::NAMES,
    profile: &PROFILE,
    tcp_port: 47520,
    discovery_port: 47521,
    magic: *b"ZGTL",
    query_magic: *b"ZGTQ",
    answer_magic: *b"ZGTA",
    joiner: b"starter-joiner",
    host: b"starter-host",
    personal: b"starter-lan",
    not_ours: "это не «Заготовка»",
    qr_scheme: SCHEME,
    qr_version: 1,
};

/// Что нужно связи и облаку в приложении от базы: местные значения и число событий.
impl kit::Device for Store {
    fn local(&self, key: &str) -> Option<String> {
        self.meta(key).ok().flatten()
    }
    fn set_local(&self, key: &str, value: &str) -> kit_core::Result<()> {
        self.set_meta(key, value).map_err(crate::error::store_err)
    }
    fn event_count(&self) -> u64 {
        Store::event_count(self).unwrap_or(0)
    }
}
