//! Команды входа так, как их зовёт экран приложения: по имени, с параметрами в JSON.
//!
//! Проверка лежит отдельно от модуля — как приложение: команды вписаны в `generate_handler!`
//! из другого крейта, а имена команд и параметров — те же, что в `api.ts` «Полки» и «Кармана».

use kit_core::{pin, AppNames};
use kit_tauri::guard::{self, Hooks, Lock, Meta};
use kit_tauri::Res;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{WebviewWindow, WebviewWindowBuilder};

const APP: AppNames = AppNames { id: "kittest", name: "Проба", name_acc: "Пробу", name_gen: "Пробы" };

#[derive(Clone, Default)]
struct MemMeta(Arc<Mutex<BTreeMap<String, String>>>);

impl Meta for MemMeta {
    fn get(&self, key: &str) -> Res<Option<String>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Res<()> {
        self.0.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
}

/// Вызов команды, как из экрана: `invoke(cmd, args)`. Ok — ответ, Err — текст ошибки.
fn invoke(window: &WebviewWindow<MockRuntime>, cmd: &str, args: Value) -> Result<Value, Value> {
    let request = InvokeRequest {
        cmd: cmd.into(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        // Адрес своей страницы приложения: с чужого адреса команды не принимаются.
        url: if cfg!(any(windows, target_os = "android")) { "http://tauri.localhost" } else { "tauri://localhost" }.parse().unwrap(),
        body: InvokeBody::Json(args),
        headers: Default::default(),
        invoke_key: INVOKE_KEY.to_string(),
    };
    get_ipc_response(window, request).map(|body| body.deserialize::<Value>().unwrap())
}

#[test]
fn the_screen_reaches_every_command_by_name() {
    let meta = MemMeta::default();
    meta.set(pin::META_HASH, &pin::hash("2580").unwrap()).unwrap();
    let app = mock_builder()
        .manage(Lock::new(APP, meta, Hooks::default()))
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
        ])
        .build(mock_context(noop_assets()))
        .unwrap();
    let window = WebviewWindowBuilder::new(&app, "main", Default::default()).build().unwrap();

    let st = invoke(&window, "lock_status", json!({})).unwrap();
    assert_eq!((&st["pin"], &st["locked"], &st["left"]), (&json!(true), &json!(true), &json!(5)));
    // Поля ответа, которые читает экран входа.
    for field in ["pin", "locked", "bio", "bio_ready", "bio_kind", "recover", "wait", "left", "recovery", "key", "notice", "error"] {
        assert!(st.get(field).is_some(), "в ответе нет поля {field}");
    }

    let st = invoke(&window, "pin_unlock", json!({ "code": "0000" })).unwrap();
    assert_eq!((&st["locked"], &st["error"], &st["left"]), (&json!(true), &json!("Неверный PIN-код"), &json!(4)));
    let st = invoke(&window, "pin_unlock", json!({ "code": "2580" })).unwrap();
    assert_eq!((&st["locked"], &st["error"]), (&json!(false), &Value::Null));

    let st = invoke(&window, "pin_set", json!({ "code": "1357", "current": "2580" })).unwrap();
    assert_eq!((&st["pin"], &st["error"]), (&json!(true), &Value::Null));
    assert_eq!(invoke(&window, "pin_set", json!({ "code": "12", "current": null })).unwrap_err(), json!("PIN-код — 4 цифры"));

    let st = invoke(&window, "app_visible", json!({ "visible": false })).unwrap();
    assert_eq!(st["locked"], json!(false));
    let st = invoke(&window, "lock_now", json!({})).unwrap();
    assert_eq!(st["locked"], json!(true));
    assert_eq!(invoke(&window, "bio_set", json!({ "on": true })).unwrap_err(), json!(guard::LOCKED));
    let st = invoke(&window, "bio_unlock", json!({})).unwrap();
    assert_eq!(st["error"], json!("Вход по отпечатку выключен"));
    // На Linux проверки личности системой нет: команда отвечает, а вход остаётся закрытым.
    let st = invoke(&window, "pin_recover", json!({})).unwrap();
    assert_eq!(st["locked"], json!(true));

    let st = invoke(&window, "pin_unlock", json!({ "code": "1357" })).unwrap();
    assert_eq!(st["locked"], json!(false));
    let st = invoke(&window, "pin_off", json!({ "current": "1357" })).unwrap();
    assert_eq!((&st["pin"], &st["locked"]), (&json!(false), &json!(false)));
}
