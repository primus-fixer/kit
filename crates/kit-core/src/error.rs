//! Ошибки общего ядра.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("PIN-код: {0}")]
    Pin(String),
    #[error("генератор случайных чисел: {0}")]
    Random(String),
    /// Резервная копия: файл не подходит, повреждён или обрезан; текст — для человека.
    #[error("{0}")]
    Backup(String),
    #[error("Пароль не подходит к этой копии")]
    BackupPassword,
    #[error("файл: {0}")]
    Io(#[from] std::io::Error),
    #[error("данные: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
