//! Общее ядро приложений на модуле kit: то, что не зависит ни от экрана, ни от системы.
//!
//! Здесь название приложения для сообщений ([`AppNames`]), PIN-код входа ([`pin`]), протокол
//! коннектора для Claude ([`mcp`]) и связь коннектора с запущенным приложением ([`link`]). Дальше сюда переезжают резервные копии, зашифрованная база
//! с журналом изменений и обмен по Wi-Fi и через облако — см. `docs/plan.md`.

pub mod error;
pub mod link;
pub mod mcp;
pub mod pin;

pub use error::{Error, Result};

/// Как приложение называется в сообщениях.
///
/// Сообщения — по-русски, поэтому название нужно в трёх падежах: «Полка», «вход в „Полку“»,
/// «PIN-код „Полки“» — и род: «Полка не запущена», «Карман не запущен».
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
    /// Женский род: «Полка не запущена, откройте её»; иначе мужской — «Карман не запущен, откройте его».
    pub feminine: bool,
}

impl AppNames {
    /// Имя переменной окружения этого приложения: `env("LOCK_AFTER_SECS")` → `POLKA_LOCK_AFTER_SECS`.
    pub fn env(&self, what: &str) -> String {
        format!("{}_{}", self.id.to_ascii_uppercase(), what)
    }

    /// Слово в роде названия: `g("запущен", "запущена")`.
    pub fn g<'a>(&self, masculine: &'a str, feminine: &'a str) -> &'a str {
        if self.feminine {
            feminine
        } else {
            masculine
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_names() {
        let app = AppNames { id: "polka", name: "Полка", name_acc: "Полку", name_gen: "Полки", feminine: true };
        assert_eq!(app.env("LOCK_AFTER_SECS"), "POLKA_LOCK_AFTER_SECS");
        assert_eq!(app.g("запущен", "запущена"), "запущена");
        assert_eq!(AppNames { feminine: false, ..app }.g("запущен", "запущена"), "запущен");
    }
}
