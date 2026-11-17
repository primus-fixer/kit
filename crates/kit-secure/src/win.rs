//! Windows: DPAPI для ключа и Windows Hello для проверки личности.

use crate::{Error, KeyNames, Result};
use windows::core::{factory, HSTRING};
use windows::Security::Credentials::UI::{UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability};
use windows::Win32::Foundation::{LocalFree, HLOCAL, HWND};
use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
use windows_future::IAsyncOperation;

fn err(e: windows::core::Error) -> Error {
    Error::Store(e.message().to_string())
}

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
}

/// Копирует результат DPAPI и освобождает память, которую выделила система.
fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if out.pbData.is_null() {
        return Vec::new();
    }
    let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
    unsafe {
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut core::ffi::c_void)));
    }
    v
}

pub fn protect(names: &KeyNames, secret: &[u8]) -> Result<Vec<u8>> {
    let input = blob(secret);
    let entropy = blob(names.dpapi_entropy);
    let label = HSTRING::from(names.dpapi_label);
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe { CryptProtectData(&input, &label, Some(&entropy as *const CRYPT_INTEGER_BLOB), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out) }.map_err(err)?;
    Ok(take(out))
}

pub fn unprotect(names: &KeyNames, data: &[u8]) -> Result<Vec<u8>> {
    let input = blob(data);
    let entropy = blob(names.dpapi_entropy);
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe { CryptUnprotectData(&input, None, Some(&entropy as *const CRYPT_INTEGER_BLOB), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out) }.map_err(err)?;
    Ok(take(out))
}

pub fn hello_available() -> bool {
    UserConsentVerifier::CheckAvailabilityAsync()
        .and_then(|op| op.join())
        .map(|a| a == UserConsentVerifierAvailability::Available)
        .unwrap_or(false)
}

pub fn hello_verify(hwnd: isize, message: &str) -> Result<bool> {
    let interop = factory::<UserConsentVerifier, IUserConsentVerifierInterop>().map_err(err)?;
    let op: IAsyncOperation<UserConsentVerificationResult> =
        unsafe { interop.RequestVerificationForWindowAsync(HWND(hwnd as *mut core::ffi::c_void), &HSTRING::from(message)) }.map_err(err)?;
    Ok(op.join().map_err(err)? == UserConsentVerificationResult::Verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::{w, PCWSTR};

    /// Так ключ защищали «Полка» до 0.11 и «Карман» до 0.15 — до общего модуля: строки вписаны
    /// прямо в вызов DPAPI.
    fn protect_as_before(entropy: &[u8], secret: &[u8], label: PCWSTR) -> Vec<u8> {
        let input = blob(secret);
        let entropy = blob(entropy);
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe { CryptProtectData(&input, label, Some(&entropy as *const CRYPT_INTEGER_BLOB), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out) }.unwrap();
        take(out)
    }

    /// Ключ, сохранённый прежней версией приложения, открывается модулем — и наоборот.
    #[test]
    fn keys_saved_before_kit_still_open() {
        let secret = [0x5Au8; 32];
        let polka = KeyNames { dpapi_entropy: b"polka-db-key-v1", dpapi_label: "Polka", keystore_alias: "polka-db-key" };
        let karman = KeyNames { dpapi_entropy: b"karman-db-key-v1", dpapi_label: "Karman", keystore_alias: "karman-db-key" };

        let old = protect_as_before(b"polka-db-key-v1", &secret, w!("Polka"));
        assert_eq!(unprotect(&polka, &old).unwrap(), secret);
        assert!(unprotect(&karman, &old).is_err(), "ключ «Полки» «Карману» не открывается");

        let old = protect_as_before(b"karman-db-key-v1", &secret, w!("Karman"));
        assert_eq!(unprotect(&karman, &old).unwrap(), secret);

        // Новая запись читается прежним способом: откат на старую версию приложения не теряет ключ.
        let new = protect(&polka, &secret).unwrap();
        let input = blob(&new);
        let entropy = blob(b"polka-db-key-v1");
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe { CryptUnprotectData(&input, None, Some(&entropy as *const CRYPT_INTEGER_BLOB), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out) }.unwrap();
        assert_eq!(take(out), secret);
    }
}
