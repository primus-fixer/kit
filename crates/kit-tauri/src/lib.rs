//! Общая часть приложений на Tauri — то, что стоит между экранами и ядром.
//!
//! Здесь вход в приложение ([`guard`]): PIN-код, отпечаток или Windows Hello, автоблокировка; связь
//! по Wi-Fi ([`wifi`]): связанные устройства, код связи, поиск и обмен в фоне, команды экрана.

pub mod guard;
pub mod wifi;

/// Ответ команды экрану: данные или текст ошибки.
pub type Res<T> = Result<T, String>;

pub(crate) fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
