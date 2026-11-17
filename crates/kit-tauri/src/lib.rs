//! Общая часть приложений на Tauri — то, что стоит между экранами и ядром.
//!
//! Пока здесь вход в приложение ([`guard`]): PIN-код, отпечаток или Windows Hello, автоблокировка.
//! Дальше сюда переезжают команды обмена, резервных копий и коннектора — см. `docs/plan.md`.

pub mod guard;

/// Ответ команды экрану: данные или текст ошибки.
pub type Res<T> = Result<T, String>;

pub(crate) fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
