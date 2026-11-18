//! Ошибки ядра — сразу понятным текстом для экрана.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("база: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("данные: {0}")]
    Json(#[from] serde_json::Error),
    #[error("неверный ключ базы")]
    WrongKey,
    /// Неверные данные: текст — для человека.
    #[error("{0}")]
    Format(String),
    /// Ошибка общего модуля (обмен, облако, копии) — его словами.
    #[error("{0}")]
    Kit(#[from] kit_core::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn bad(msg: impl Into<String>) -> Error {
    Error::Format(msg.into())
}

/// Для общего модуля: ошибка базы приложения.
pub(crate) fn store_err(e: Error) -> kit_core::Error {
    match e {
        Error::Kit(k) => k,
        other => kit_core::Error::Store(other.to_string()),
    }
}
