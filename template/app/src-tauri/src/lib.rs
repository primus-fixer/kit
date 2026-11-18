//! «Заготовка»: мост между экранами и ядром `starter-core`.
//!
//! Общее — из kit: вход по PIN-коду, отпечатку и Windows Hello (`kit_tauri::guard`), ключ базы в хранилище
//! системы (`kit_secure`), обмен по Wi-Fi (`kit_tauri::wifi`) и через облако (`kit_tauri::cloud`). Своё —
//! команды записей (в заготовке — заметки). Пока приложение заблокировано, команды с данными отвечают `locked`.

use kit_tauri::guard::{self, Lock};
use serde::{Deserialize, Serialize};
use starter_core::db::Store;
use starter_core::model::{Note, NOTE};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use tauri::{Manager, State};

mod cloud;
mod lan;
mod names;

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

struct AppState {
    /// База — общая с фоновым обменом по Wi-Fi и через облако.
    store: Arc<Mutex<Store>>,
    guard: Lock,
}

impl AppState {
    /// База для команд с данными: пока приложение заблокировано, ответ — `locked`.
    fn store(&self) -> Res<MutexGuard<'_, Store>> {
        self.guard.check()?;
        self.store.lock().map_err(|_| "база занята — попробуйте ещё раз".to_string())
    }
}

/// Местные значения базы — для входа по PIN-коду: там лежат его хеш и счётчик ошибок.
struct StoreMeta(Arc<Mutex<Store>>);

impl guard::Meta for StoreMeta {
    fn get(&self, key: &str) -> Res<Option<String>> {
        self.0.lock().map_err(err)?.meta(key).map_err(err)
    }

    fn set(&self, key: &str, value: &str) -> Res<()> {
        self.0.lock().map_err(err)?.set_meta(key, value).map_err(err)
    }
}

// ——— Устройство и ключ базы ———

/// Имя устройства для связи и облака: имя компьютера в системе, на телефоне — «Телефон».
fn device_name() -> String {
    if cfg!(mobile) {
        return "Телефон".into();
    }
    let raw = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).ok().or_else(|| std::fs::read_to_string("/etc/hostname").ok()).unwrap_or_default();
    let name: String = raw.trim().chars().filter(|c| !c.is_control()).take(40).collect();
    if name.is_empty() {
        "Компьютер".into()
    } else {
        name
    }
}

#[derive(Serialize, Deserialize)]
struct Identity {
    id: String,
}

/// Номер этого устройства: создаётся один раз и больше не меняется (по нему другие устройства узнают события).
fn load_identity(dir: &Path) -> Res<Identity> {
    let path = dir.join("device.json");
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(id) = serde_json::from_str::<Identity>(&text) {
            if !id.id.is_empty() {
                return Ok(id);
            }
        }
    }
    let id = Identity { id: starter_core::new_id().map_err(err)? };
    write_atomic(&path, serde_json::to_string_pretty(&id).map_err(err)?.as_bytes())?;
    Ok(id)
}

fn write_atomic(path: &Path, data: &[u8]) -> Res<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data).map_err(err)?;
    std::fs::rename(&tmp, path).map_err(err)
}

/// Ключ базы — в файле `key.sec`, зашифрованный средствами системы (DPAPI, Android Keystore).
/// Новый создаётся только для новой базы: если база есть, а ключа нет, — это ошибка, а не повод начать с пустой.
fn load_key(dir: &Path, db: &Path) -> Res<[u8; 32]> {
    let path = dir.join("key.sec");
    match std::fs::read(&path) {
        Ok(blob) => {
            let k = names::unprotect(&blob).map_err(|e| format!("ключ базы: не достать из хранилища системы: {e}"))?;
            return <[u8; 32]>::try_from(k.as_slice()).map_err(|_| "ключ базы повреждён".to_string());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(err(e)),
    }
    if db.exists() {
        return Err("база есть, а ключа к ней нет — восстановите файл key.sec".into());
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(err)?;
    write_atomic(&path, &names::protect(&key).map_err(err)?)?;
    Ok(key)
}

// ——— Записи (в заготовке — заметки: замените своими) ———

#[tauri::command]
fn notes(state: State<'_, AppState>) -> Res<Vec<Note>> {
    let mut all: Vec<Note> = state.store()?.list(NOTE).map_err(err)?;
    all.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| b.id.cmp(&a.id)));
    Ok(all)
}

#[tauri::command]
fn note_save(state: State<'_, AppState>, mut note: Note) -> Res<Note> {
    note.text = note.text.trim().to_string();
    if note.text.is_empty() {
        return Err("Пустая заметка".into());
    }
    if note.id.is_empty() {
        note.id = starter_core::new_id().map_err(err)?;
        note.created = starter_core::now_secs();
    }
    state.store()?.put(NOTE, &note.id.clone(), &note).map_err(err)?;
    Ok(note)
}

#[tauri::command]
fn note_delete(state: State<'_, AppState>, id: String) -> Res<()> {
    state.store()?.delete(NOTE, &id).map_err(err)
}

#[derive(Serialize)]
struct DeviceInfo {
    name: String,
    version: &'static str,
    key_method: &'static str,
    events: u64,
}

#[tauri::command]
fn device_info(state: State<'_, AppState>) -> Res<DeviceInfo> {
    let events = state.store()?.event_count().map_err(err)?;
    Ok(DeviceInfo { name: device_name(), version: env!("CARGO_PKG_VERSION"), key_method: kit_secure::method(), events })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // Второй запуск открывает уже работающее приложение.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
    }));
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_barcode_scanner::init()).plugin(tauri_plugin_biometric::init());
    builder
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let db = dir.join("starter.db");
            let me = load_identity(&dir)?;
            let key = load_key(&dir, &db)?;
            let store = Arc::new(Mutex::new(Store::open(&db, &key, &me.id).map_err(err)?));
            let handle = app.handle().clone();
            let hooks = guard::Hooks {
                // Пока приложение заблокировано, по прежнему коду связи не подключиться.
                on_lock: Box::new(move || lan::on_lock(&handle)),
                // Вернулись — заодно обменяться с облаком, если давно не обменивались.
                on_resume: Box::new(cloud::resumed),
            };
            // С PIN-кодом приложение открывается заблокированным.
            let guard = Lock::new(names::APP, StoreMeta(store.clone()), hooks);
            app.manage(guard.clone());
            app.manage(cloud::Sky::new(cloud::settings(), store.clone(), None));
            app.manage(lan::Wifi::new(&starter_core::lan::LAN, &names::KEYS, store.clone(), me.id.clone(), device_name()));
            app.manage(AppState { store, guard });
            lan::start(app.handle().clone());
            cloud::start(app.handle().clone());
            Ok(())
        })
        // Свернули: через 5 минут в фоне — блокировка (с PIN-кодом); вернулись — обмен с облаком.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Focused(focused) = event {
                guard::on_focus(window.app_handle(), *focused)
            }
        })
        .invoke_handler(tauri::generate_handler![
            guard::lock_status,
            guard::pin_unlock,
            guard::pin_set,
            guard::pin_off,
            guard::bio_unlock,
            guard::bio_set,
            guard::pin_recover,
            guard::lock_now,
            guard::app_visible,
            notes,
            note_save,
            note_delete,
            device_info,
            lan::lan_status,
            lan::lan_scan,
            lan::lan_code_start,
            lan::lan_code_stop,
            lan::lan_pair,
            lan::lan_pair_qr,
            lan::lan_sync_now,
            lan::lan_unpair,
            cloud::cloud_status,
            cloud::cloud_app_passwords,
            cloud::cloud_dav_login,
            cloud::cloud_password,
            cloud::cloud_sync_now,
            cloud::cloud_every,
            cloud::cloud_disconnect,
        ])
        .run(tauri::generate_context!())
        .expect("приложение не запустилось");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_created_once_and_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("starter.db");
        let k1 = load_key(dir.path(), &db).unwrap();
        assert_eq!(load_key(dir.path(), &db).unwrap(), k1);
        // База есть, а ключ пропал — ошибка, а не новый ключ к пустой базе.
        std::fs::write(&db, b"x").unwrap();
        std::fs::remove_file(dir.path().join("key.sec")).unwrap();
        assert!(load_key(dir.path(), &db).is_err());
    }

    #[test]
    fn device_id_stays() {
        let dir = tempfile::tempdir().unwrap();
        let a = load_identity(dir.path()).unwrap().id;
        assert_eq!(load_identity(dir.path()).unwrap().id, a);
        assert_eq!(a.len(), 16);
    }
}
