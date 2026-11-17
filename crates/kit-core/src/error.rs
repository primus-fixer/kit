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
    /// Пакет канала не расшифровался: не тот ключ, пакет изменён, повторён или не на своём месте.
    #[error("пакет не расшифровался")]
    Decrypt,
    /// Связь по Wi-Fi: что случилось ([`crate::lan::LanFail`]) и что сказать человеку.
    #[error("{1}")]
    Lan(crate::lan::LanFail, String),
    /// Облако: что случилось ([`crate::cloud::CloudFail`]) и что сказать человеку.
    #[error("{1}")]
    Cloud(crate::cloud::CloudFail, String),
    #[error("Пароль не подходит: облако зашифровано другим паролем копий")]
    CloudPassword,
}

pub type Result<T> = std::result::Result<T, Error>;
