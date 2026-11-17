//! Облака приложений на модуле kit: клиент WebDAV ([`dav`]) — Яндекс Диск по паролю приложения или свой
//! сервер (Nextcloud, NAS) — и запросы к облаку с понятными ошибками ([`http`]), на которых приложение
//! строит и свои облака (у «Кармана» — Яндекс Диск и Google Drive по коду).
//!
//! В облако уходят файлы, уже зашифрованные (`kit_core::cloud`): облако их не читает.

pub mod dav;
pub mod http;
#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use http::now;

#[cfg(test)]
mod tests;
