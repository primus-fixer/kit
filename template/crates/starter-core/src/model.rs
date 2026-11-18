//! Записи приложения. В заготовке — заметка: замените своими видами записей.
//!
//! Запись хранится в базе как «вид, номер, содержимое» (JSON): новое поле — просто поле с `#[serde(default)]`,
//! новая версия базы не нужна. Снимок записи — поле `image` (номер снимка): так обмен знает, что его передать.

use serde::{Deserialize, Serialize};

/// Вид записи «заметка» в базе. **У выпущенного приложения не менять.**
pub const NOTE: &str = "note";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub text: String,
    /// Когда создана: секунды Unix.
    pub created: i64,
    /// Снимок, если есть: номер в базе.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}
