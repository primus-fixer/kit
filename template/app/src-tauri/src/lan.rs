//! Связь по Wi-Fi — общая, из kit (`kit_tauri::wifi`): команды экрана, фоновый обмен, связанные устройства.
//! Своё — настройки связи (`starter_core::lan::LAN`).

use starter_core::db::Store;
use tauri::AppHandle;

kit_tauri::wifi_commands!(Store);

/// Связь по Wi-Fi в приложении — состояние для `app.manage`.
pub type Wifi = kit_tauri::wifi::Wifi<Store>;

/// Фон: открыть порты, когда нужно, находить связанные устройства и обмениваться.
pub fn start(app: AppHandle) {
    kit_tauri::wifi::start::<_, Store>(app);
}

/// Приложение заблокировалось: код связи больше не действует.
pub fn on_lock(app: &AppHandle) {
    kit_tauri::wifi::on_lock::<_, Store>(app);
}
