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
    /// Обмен: другая сторона прислала не то, что ожидалось.
    #[error("{0}")]
    Format(String),
    /// Обмен: другое устройство не смогло принять записи (старая версия).
    #[error("{0}")]
    Peer(String),
    /// Ошибка базы приложения — её текст.
    #[error("{0}")]
    Store(String),
}

pub type Result<T> = std::result::Result<T, Error>;
