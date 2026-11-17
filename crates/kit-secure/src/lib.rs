//! Ключ базы приложения в хранилище системы и проверка, что за устройством владелец.
//!
//! - Windows: ключ шифруется учётной записью пользователя Windows (DPAPI);
//!   проверка личности — Windows Hello (отпечаток, лицо или PIN Windows).
//! - Android: ключ шифруется ключом AES из Android Keystore, который не покидает телефон.
//!   Отпечаток на Android — через плагин biometric в самом приложении.
//! - Остальные системы (Linux для разработки): ключ хранится как есть.
//!
//! Чем приложения отличаются друг от друга, описывает [`KeyNames`].

#[cfg(target_os = "android")]
mod android;
#[cfg(windows)]
mod win;

/// Строки, которыми ключ привязан к приложению.
///
/// **Менять их у выпущенного приложения нельзя:** с другими строками система не расшифрует то, что
/// уже лежит на устройстве, — ключ базы, пароль облака, ключи связи. У нового приложения строки
/// выбирают один раз.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyNames {
    /// Windows: дополнительная «соль» DPAPI. Чужая программа под той же учётной записью без неё
    /// ключ простым вызовом не расшифрует.
    pub dpapi_entropy: &'static [u8],
    /// Windows: подпись записи DPAPI — видна в самой записи, на расшифровку не влияет.
    pub dpapi_label: &'static str,
    /// Android: имя ключа в Android Keystore.
    pub keystore_alias: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("хранилище ключей системы: {0}")]
    Store(String),
    #[error("защищённый ключ повреждён")]
    Format,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Чем защищён ключ на этой системе: `dpapi`, `android-keystore` или `file`.
pub fn method() -> &'static str {
    if cfg!(windows) {
        "dpapi"
    } else if cfg!(target_os = "android") {
        "android-keystore"
    } else {
        "file"
    }
}

/// Можно ли защитить ключ средствами системы.
pub fn supported() -> bool {
    cfg!(any(windows, target_os = "android"))
}

/// Шифрует секрет так, что расшифровать его можно только на этом устройстве
/// (на Windows — ещё и только под этой учётной записью).
pub fn protect(names: &KeyNames, secret: &[u8]) -> Result<Vec<u8>> {
    #[cfg(windows)]
    return win::protect(names, secret);
    #[cfg(target_os = "android")]
    return android::protect(names, secret);
    #[cfg(not(any(windows, target_os = "android")))]
    {
        let _ = names;
        Ok(secret.to_vec())
    }
}

pub fn unprotect(names: &KeyNames, blob: &[u8]) -> Result<Vec<u8>> {
    #[cfg(windows)]
    return win::unprotect(names, blob);
    #[cfg(target_os = "android")]
    return android::unprotect(names, blob);
    #[cfg(not(any(windows, target_os = "android")))]
    {
        let _ = names;
        Ok(blob.to_vec())
    }
}

/// Настроен ли Windows Hello (отпечаток, лицо или PIN Windows).
pub fn hello_available() -> bool {
    #[cfg(windows)]
    return win::hello_available();
    #[cfg(not(windows))]
    false
}

/// Просит Windows Hello подтвердить личность. Окно запроса — поверх окна `hwnd`.
/// Блокирует поток до ответа, поэтому вызывать не из главного потока.
pub fn hello_verify(hwnd: isize, message: &str) -> Result<bool> {
    #[cfg(windows)]
    return win::hello_verify(hwnd, message);
    #[cfg(not(windows))]
    {
        let _ = (hwnd, message);
        Err(Error::Store("Windows Hello есть только в Windows".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: KeyNames = KeyNames { dpapi_entropy: b"kit-test-key-v1", dpapi_label: "KitTest", keystore_alias: "kit-test-key" };

    #[test]
    fn roundtrip_on_this_system() {
        let secret = [7u8; 32];
        let blob = protect(&NAMES, &secret).unwrap();
        assert_eq!(unprotect(&NAMES, &blob).unwrap(), secret);
    }

    /// Секрет одного приложения другое не откроет.
    #[cfg(windows)]
    #[test]
    fn other_names_do_not_open() {
        let other = KeyNames { dpapi_entropy: b"kit-other-key-v1", ..NAMES };
        let blob = protect(&NAMES, &[7u8; 32]).unwrap();
        assert!(unprotect(&other, &blob).is_err());
    }
}
