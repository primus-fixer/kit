//! Ошибки общего ядра.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("PIN-код: {0}")]
    Pin(String),
    #[error("генератор случайных чисел: {0}")]
    Random(String),
}

pub type Result<T> = std::result::Result<T, Error>;
