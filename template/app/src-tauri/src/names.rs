//! Чем приложение отличается от других на общем модуле kit: название для сообщений и строки,
//! которыми ключи привязаны к приложению.
//!
//! **Строки ключей у выпущенного приложения менять нельзя.** С другими строками система не расшифрует ключ
//! базы, пароль облака и ключи связи, которые уже лежат на устройствах, — приложение не откроет свою базу.

use kit_core::AppNames;
use kit_secure::KeyNames;

pub const APP: AppNames = starter_core::NAMES;

pub const KEYS: KeyNames = KeyNames { dpapi_entropy: b"starter-db-key-v1", dpapi_label: "Starter", keystore_alias: "starter-db-key" };

/// Шифрует секрет средствами системы: DPAPI на Windows, Android Keystore на телефоне;
/// на Linux (разработка) секрет остаётся как есть.
pub fn protect(secret: &[u8]) -> kit_secure::Result<Vec<u8>> {
    kit_secure::protect(&KEYS, secret)
}

pub fn unprotect(blob: &[u8]) -> kit_secure::Result<Vec<u8>> {
    kit_secure::unprotect(&KEYS, blob)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_roundtrip() {
        let blob = protect(&[7u8; 32]).unwrap();
        assert_eq!(unprotect(&blob).unwrap(), [7u8; 32]);
    }
}
