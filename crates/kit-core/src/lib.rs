//! Общее ядро приложений на модуле kit: то, что не зависит ни от экрана, ни от системы.
//!
//! Пока здесь название приложения для сообщений ([`AppNames`]) и PIN-код входа ([`pin`]).
//! Дальше сюда переезжают зашифрованная база с журналом изменений, резервные копии, обмен
//! по Wi-Fi и через облако и коннектор для Claude — см. `docs/plan.md`.

pub mod error;
pub mod pin;

pub use error::{Error, Result};

/// Как приложение называется в сообщениях.
///
/// Сообщения — по-русски, поэтому название нужно в трёх падежах: «Полка», «вход в „Полку“»,
/// «PIN-код „Полки“».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppNames {
    /// Латиницей, маленькими буквами: `polka`. Из него — имена переменных окружения для проверок.
    pub id: &'static str,
    /// Именительный падеж: «Полка».
    pub name: &'static str,
    /// Винительный падеж: «Полку» (вход в «Полку»).
    pub name_acc: &'static str,
    /// Родительный падеж: «Полки» (PIN-код «Полки»).
    pub name_gen: &'static str,
}

impl AppNames {
    /// Имя переменной окружения этого приложения: `env("LOCK_AFTER_SECS")` → `POLKA_LOCK_AFTER_SECS`.
    pub fn env(&self, what: &str) -> String {
        format!("{}_{}", self.id.to_ascii_uppercase(), what)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_names() {
        let app = AppNames { id: "polka", name: "Полка", name_acc: "Полку", name_gen: "Полки" };
        assert_eq!(app.env("LOCK_AFTER_SECS"), "POLKA_LOCK_AFTER_SECS");
    }
}
