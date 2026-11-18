//! Ядро «Заготовки» — приложения на общем модуле kit (сделано из заготовки kit, template/).
//!
//! Всё общее — в kit; здесь только то, чем приложение отличается от других:
//! - [`NAMES`] — название в сообщениях;
//! - `model` — записи приложения (в заготовке — заметки: замените своими);
//! - `db` — база на `kit_core::db`: записи «вид, номер, содержимое» и снимки;
//! - `sync`, `lan`, `cloud`, `backup` — настройки обмена по Wi-Fi, облака и копий: порты, метки, расширения.
//!   **У выпущенного приложения их не менять:** устройства перестанут узнавать друг друга и читать облако.

pub mod backup;
pub mod cloud;
pub mod db;
pub mod error;
pub mod lan;
pub mod model;
pub mod sync;

pub use error::{Error, Result};

/// Как приложение называется в сообщениях общего модуля (вход, копии, облако, связь).
pub const NAMES: kit_core::AppNames = kit_core::AppNames { id: "starter", name: "Заготовка", name_acc: "Заготовку", name_gen: "Заготовки", feminine: true };

/// Новый номер записи: 16 шестнадцатеричных знаков.
pub fn new_id() -> Result<String> {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).map_err(|e| Error::Format(e.to_string()))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Сейчас: секунды Unix.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests;
