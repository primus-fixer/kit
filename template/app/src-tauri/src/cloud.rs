//! Облако — общее, из kit (`kit_tauri::cloud`): Яндекс Диск по паролю приложения или свой сервер WebDAV,
//! пароль копий, обмен в фоне, команды экрана. Своё — формат облака (`starter_core::cloud::FORMAT`).

use starter_core::cloud::FORMAT;
use starter_core::db::Store;
use tauri::AppHandle;

kit_tauri::cloud_commands!(Store);

/// Облако в приложении — состояние для `app.manage`.
pub type Sky = kit_tauri::cloud::Sky<Store>;

/// Где что лежит в базе. **У выпущенного приложения не менять:** вход в облако на устройствах пропадёт.
pub fn settings() -> kit_tauri::cloud::Settings {
    kit_tauri::cloud::Settings { cloud: &FORMAT, backup: &starter_core::backup::BACKUP, keys: &crate::names::KEYS, secret: "cloud.password", backup_key: starter_core::backup::META_KEY }
}

/// Обмен в фоне.
pub fn start(app: AppHandle) {
    kit_tauri::cloud::start::<_, Store>(app);
}

/// Вернулись в приложение: обменяться, если давно не обменивались.
pub fn resumed() {
    kit_tauri::cloud::resumed();
}
