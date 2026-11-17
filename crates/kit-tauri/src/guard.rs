//! Вход в приложение: PIN-код, отпечаток (Android) или Windows Hello, автоблокировка.
//!
//! Пока приложение заблокировано, его команды с данными отвечают [`LOCKED`], и экран показывает вход.
//! Забытый PIN сбрасывается после проверки блокировкой устройства: экраном блокировки телефона
//! или Windows Hello.
//!
//! Приложение создаёт [`Lock`] при запуске, отдаёт его Tauri (`app.manage(lock.clone())`),
//! вписывает команды этого модуля в `tauri::generate_handler!` и сообщает о фокусе окна
//! ([`on_focus`]). Перед работой с данными оно спрашивает [`Lock::check`].

use crate::{err, Res};
use kit_core::{pin, AppNames};
use serde::Serialize;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Ответ команд с данными, пока приложение заблокировано.
pub const LOCKED: &str = "locked";
/// Событие экрану: приложение заблокировалось само.
pub const LOCKED_EVENT: &str = "locked";
/// Ответ команд входа, пока приложение запускается.
pub const STARTING: &str = "приложение ещё открывается — повторите через несколько секунд";
/// Через сколько в фоне снова спрашивать вход.
const LOCK_AFTER: Duration = Duration::from_secs(5 * 60);
/// Сколько действует подтверждение устройством для нового PIN-кода.
const RECOVERY: Duration = Duration::from_secs(5 * 60);

/// Местные настройки устройства (таблица `meta` базы): здесь лежат хеш PIN-кода и счётчик ошибок.
/// В журнал изменений они не попадают — у каждого устройства свой PIN.
pub trait Meta: Send + Sync + 'static {
    fn get(&self, key: &str) -> Res<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Res<()>;
}

/// Что приложение делает, когда вход меняет состояние.
pub struct Hooks {
    /// Приложение заблокировалось: остановить то, что без входа работать не должно
    /// (например, показанный код связи).
    pub on_lock: Box<dyn Fn() + Send + Sync>,
    /// В приложение вернулись (окно получило фокус).
    pub on_resume: Box<dyn Fn() + Send + Sync>,
}

impl Default for Hooks {
    fn default() -> Self {
        Hooks { on_lock: Box::new(|| {}), on_resume: Box::new(|| {}) }
    }
}

#[derive(Default)]
struct State {
    locked: bool,
    pin: bool,
    hidden_since: Option<Instant>,
    recovery_until: Option<Instant>,
}

impl State {
    fn recovering(&self) -> bool {
        self.recovery_until.is_some_and(|t| Instant::now() < t)
    }
}

struct Inner {
    names: AppNames,
    meta: Box<dyn Meta>,
    hooks: Hooks,
    state: Mutex<State>,
    /// Проверки PIN-кода идут по одной: счётчик ошибок не обойти одновременными запросами.
    pin_gate: Mutex<()>,
    /// Сообщение при запуске — экран забирает его один раз.
    notice: Mutex<Option<String>>,
}

/// Вход в приложение. Копии указывают на одно и то же состояние.
#[derive(Clone)]
pub struct Lock(Arc<Inner>);

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Lock {
    /// С PIN-кодом приложение открывается заблокированным.
    pub fn new(names: AppNames, meta: impl Meta, hooks: Hooks) -> Lock {
        let pin = meta.get(pin::META_HASH).ok().flatten().is_some_and(|h| !h.is_empty());
        let state = State { locked: pin, pin, ..Default::default() };
        Lock(Arc::new(Inner { names, meta: Box::new(meta), hooks, state: Mutex::new(state), pin_gate: Mutex::new(()), notice: Mutex::new(None) }))
    }

    /// Сообщение, которое экран входа покажет один раз (например, что прежнюю базу не удалось открыть).
    pub fn set_notice(&self, text: Option<String>) {
        if let Ok(mut n) = self.0.notice.lock() {
            *n = text;
        }
    }

    /// Заблокировано ли приложение сейчас.
    pub fn locked(&self) -> bool {
        self.0.state.lock().map(|g| g.locked).unwrap_or(true)
    }

    /// Для команд с данными: пока приложение заблокировано — ошибка [`LOCKED`].
    pub fn check(&self) -> Res<()> {
        if self.state()?.locked {
            return Err(LOCKED.into());
        }
        Ok(())
    }

    /// Задан ли PIN-код.
    pub fn has_pin(&self) -> bool {
        self.0.state.lock().map(|g| g.pin).unwrap_or(false)
    }

    fn state(&self) -> Res<MutexGuard<'_, State>> {
        self.0.state.lock().map_err(err)
    }

    fn meta_u64(&self, key: &str) -> u64 {
        self.0.meta.get(key).ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    fn meta_is(&self, key: &str, value: &str) -> bool {
        self.0.meta.get(key).ok().flatten().as_deref() == Some(value)
    }

    fn reset_fails(&self) -> Res<()> {
        self.0.meta.set(pin::META_FAILS, "0")?;
        self.0.meta.set(pin::META_UNTIL, "0")
    }

    /// Проверяет PIN с учётом пауз после ошибок. Ошибка — текст для экрана.
    fn check_pin(&self, code: &str) -> Result<(), String> {
        let _one_at_a_time = self.0.pin_gate.lock().map_err(err)?;
        let now = now_secs();
        if self.meta_u64(pin::META_UNTIL) > now {
            return Err("Слишком много попыток, подождите".into());
        }
        let Some(hash) = self.0.meta.get(pin::META_HASH)?.filter(|h| !h.is_empty()) else {
            return Err("PIN-код не задан".into());
        };
        if pin::verify(code, &hash) {
            return self.reset_fails();
        }
        let fails = self.meta_u64(pin::META_FAILS) as u32 + 1;
        self.0.meta.set(pin::META_FAILS, &fails.to_string())?;
        let pause = pin::pause_after(fails);
        if pause > 0 {
            self.0.meta.set(pin::META_UNTIL, &(now + pause).to_string())?;
        }
        Err("Неверный PIN-код".into())
    }

    fn unlock(&self) -> Res<()> {
        let mut g = self.state()?;
        g.locked = false;
        g.hidden_since = None;
        Ok(())
    }

    /// Вход по PIN-коду. Ok(Some(текст)) — не вошли, текст для экрана.
    fn pin_unlock(&self, code: &str) -> Res<Option<String>> {
        match self.check_pin(code) {
            Ok(()) => {
                self.unlock()?;
                Ok(None)
            }
            Err(e) => Ok(Some(e)),
        }
    }

    /// Новый PIN. Если PIN уже есть, нужен текущий — или подтверждение устройством («Забыли PIN?»).
    fn pin_set(&self, code: &str, current: Option<&str>) -> Res<Option<String>> {
        let (has, recovering) = {
            let g = self.state()?;
            (g.pin, g.recovering())
        };
        if has && !recovering {
            let Some(current) = current else { return Ok(Some("Введите текущий PIN-код".into())) };
            if let Err(e) = self.check_pin(current) {
                return Ok(Some(e));
            }
        }
        self.0.meta.set(pin::META_HASH, &pin::hash(code).map_err(err)?)?;
        self.reset_fails()?;
        let mut g = self.state()?;
        g.pin = true;
        g.locked = false;
        g.recovery_until = None;
        Ok(None)
    }

    /// Выключает вход по PIN-коду (и отпечатку). Нужен текущий PIN.
    fn pin_off(&self, current: Option<&str>) -> Res<Option<String>> {
        if !self.state()?.recovering() {
            let Some(current) = current else { return Ok(Some("Введите текущий PIN-код".into())) };
            if let Err(e) = self.check_pin(current) {
                return Ok(Some(e));
            }
        }
        self.0.meta.set(pin::META_HASH, "")?;
        self.0.meta.set(pin::META_BIO, "0")?;
        self.reset_fails()?;
        let mut g = self.state()?;
        g.pin = false;
        g.locked = false;
        g.recovery_until = None;
        Ok(None)
    }

    /// Устройство подтвердило владельца («Забыли PIN?»): вход открыт, и можно задать новый PIN.
    fn recovered(&self) -> Res<()> {
        self.reset_fails()?;
        let mut g = self.state()?;
        g.locked = false;
        g.hidden_since = None;
        g.recovery_until = Some(Instant::now() + RECOVERY);
        Ok(())
    }

    /// Блокирует приложение, если PIN-код задан.
    fn lock_now(&self) -> Res<()> {
        let mut g = self.state()?;
        if g.pin {
            g.locked = true;
            drop(g);
            (self.0.hooks.on_lock)();
        }
        Ok(())
    }

    /// Окно потеряло или получило фокус. `true` — приложение только что заблокировалось:
    /// слишком долго было в фоне.
    fn focus_changed(&self, focused: bool, lock_after: Duration) -> bool {
        let Ok(mut g) = self.0.state.lock() else { return false };
        if !focused {
            if g.hidden_since.is_none() {
                g.hidden_since = Some(Instant::now());
            }
            return false;
        }
        let since = g.hidden_since.take();
        let lock = since.is_some_and(|s| g.pin && !g.locked && s.elapsed() >= lock_after);
        if lock {
            g.locked = true;
        }
        drop(g);
        // Вернулись в приложение — например, пора обменяться с облаком.
        (self.0.hooks.on_resume)();
        if lock {
            // Пока приложение заблокировано, по прежнему коду связи не подключиться.
            (self.0.hooks.on_lock)();
        }
        lock
    }

    fn lock_after(&self) -> Duration {
        // Для сквозных проверок в отладочной сборке время можно сократить.
        #[cfg(debug_assertions)]
        if let Some(s) = std::env::var(self.0.names.env("LOCK_AFTER_SECS")).ok().and_then(|v| v.parse().ok()) {
            return Duration::from_secs(s);
        }
        LOCK_AFTER
    }

    fn status(&self, bio: &Bio, error: Option<String>) -> Res<LockStatus> {
        let (pin, locked, recovery) = {
            let g = self.state()?;
            (g.pin, g.locked, g.recovering())
        };
        let fails = self.meta_u64(pin::META_FAILS) as u32;
        Ok(LockStatus {
            pin,
            locked,
            bio: pin && self.meta_is(pin::META_BIO, "1"),
            bio_ready: bio.ready,
            bio_kind: bio.kind,
            recover: bio.recover,
            wait: self.meta_u64(pin::META_UNTIL).saturating_sub(now_secs()),
            left: pin::tries_left(fails),
            recovery,
            key: kit_secure::method(),
            notice: self.0.notice.lock().ok().and_then(|mut n| n.take()),
            error,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct LockStatus {
    /// PIN-код задан.
    pin: bool,
    locked: bool,
    /// Вход по отпечатку или Windows Hello включён.
    bio: bool,
    /// Отпечаток или Windows Hello доступны на устройстве сейчас.
    bio_ready: bool,
    /// fingerprint, face или hello.
    bio_kind: &'static str,
    /// Чем подтвердить себя, если PIN забыт: device (экран блокировки телефона), hello или пусто.
    recover: &'static str,
    /// Секунд до следующей попытки ввода PIN.
    wait: u64,
    /// Сколько ещё можно ошибиться до паузы.
    left: u32,
    /// Устройство подтвердило владельца — можно задать новый PIN без старого.
    recovery: bool,
    /// Чем защищён ключ базы: dpapi, android-keystore или file.
    key: &'static str,
    /// Сообщение при запуске, например если прежнюю базу не удалось открыть.
    notice: Option<String>,
    error: Option<String>,
}

/// Что умеет устройство: отпечаток или Windows Hello и сброс PIN.
struct Bio {
    ready: bool,
    kind: &'static str,
    recover: &'static str,
}

/// Только для сквозных проверок на Linux в отладочной сборке: `<ПРИЛОЖЕНИЕ>_FAKE_AUTH` — отпечаток,
/// `<ПРИЛОЖЕНИЕ>_FAKE_DEVICE` — блокировка устройства; значение `cancel` — человек отменил проверку.
#[cfg(all(debug_assertions, not(any(target_os = "android", target_os = "ios", windows))))]
fn fake_auth(names: &AppNames, device: bool) -> Option<String> {
    let own = if device { std::env::var(names.env("FAKE_DEVICE")).ok() } else { None };
    own.or_else(|| std::env::var(names.env("FAKE_AUTH")).ok())
}

#[allow(unused_variables)]
async fn bio_info<R: Runtime>(app: &AppHandle<R>, names: &AppNames) -> Bio {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        use tauri_plugin_biometric::{BiometricExt, BiometryType};
        let a = app.clone();
        // В фоновом потоке: вызов плагина ждёт ответа от Android.
        let status = tauri::async_runtime::spawn_blocking(move || a.biometric().status().ok().map(|s| (s.is_available, matches!(s.biometry_type, BiometryType::FaceID)))).await;
        let (ready, kind) = match status {
            Ok(Some((true, face))) => (true, if face { "face" } else { "fingerprint" }),
            _ => (false, ""),
        };
        Bio { ready, kind, recover: "device" }
    }
    #[cfg(windows)]
    {
        let ready = tauri::async_runtime::spawn_blocking(kit_secure::hello_available).await.unwrap_or(false);
        Bio { ready, kind: "hello", recover: if ready { "hello" } else { "" } }
    }
    #[cfg(not(any(target_os = "android", target_os = "ios", windows)))]
    {
        #[cfg(debug_assertions)]
        if fake_auth(names, false).is_some() {
            return Bio { ready: true, kind: "fingerprint", recover: "device" };
        }
        Bio { ready: false, kind: "", recover: "" }
    }
}

/// Проверка личности системой. `device` разрешает экран блокировки телефона (сброс PIN).
/// Ok(false) — человек отменил проверку.
#[allow(unused_variables)]
async fn system_auth<R: Runtime>(app: &AppHandle<R>, names: &AppNames, reason: &str, device: bool) -> Res<bool> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        use tauri::plugin::mobile::PluginInvokeError;
        use tauri_plugin_biometric::{AuthOptions, BiometricExt, Error};
        let a = app.clone();
        let reason = reason.to_string();
        let options = AuthOptions {
            allow_device_credential: device,
            cancel_title: Some(if device { "Отмена".into() } else { "Ввести PIN".into() }),
            title: Some(names.name.into()),
            confirmation_required: Some(false),
            ..Default::default()
        };
        // Ответ переводим в простые строки ещё в фоновом потоке.
        let result: Result<(), (Option<String>, String)> = tauri::async_runtime::spawn_blocking(move || match a.biometric().authenticate(reason, options) {
            Ok(()) => Ok(()),
            Err(Error::PluginInvoke(PluginInvokeError::InvokeRejected(e))) => Err((e.code, e.message.unwrap_or_default())),
            Err(e) => Err((None, e.to_string())),
        })
        .await
        .map_err(err)?;
        match result {
            Ok(()) => Ok(true),
            Err((code, msg)) => match code.as_deref() {
                Some("userCancel" | "systemCancel" | "appCancel" | "authenticationFailed") => Ok(false),
                Some("passcodeNotSet") => Err("На телефоне не задана блокировка экрана. Включите её в настройках телефона и повторите.".into()),
                Some("biometryLockout") => Err("Слишком много попыток отпечатка. Введите PIN-код или попробуйте позже.".into()),
                Some("biometryNotEnrolled") => Err("На телефоне не добавлен отпечаток пальца.".into()),
                Some("biometryNotAvailable") => Err("Сканер отпечатков сейчас недоступен.".into()),
                _ => Err(if msg.is_empty() { "Проверка не удалась".into() } else { msg }),
            },
        }
    }
    #[cfg(windows)]
    {
        if !tauri::async_runtime::spawn_blocking(kit_secure::hello_available).await.unwrap_or(false) {
            return Err("Windows Hello не настроен. Включите его: Параметры → Учётные записи → Варианты входа — и повторите.".into());
        }
        let window = app.get_webview_window("main").ok_or("нет окна приложения")?;
        let hwnd = window.hwnd().map_err(err)?.0 as isize;
        let reason = reason.to_string();
        tauri::async_runtime::spawn_blocking(move || kit_secure::hello_verify(hwnd, &reason)).await.map_err(err)?.map_err(err)
    }
    #[cfg(not(any(target_os = "android", target_os = "ios", windows)))]
    {
        #[cfg(debug_assertions)]
        if let Some(mode) = fake_auth(names, device) {
            return Ok(mode != "cancel");
        }
        Err("Проверка отпечатком на этой системе недоступна".into())
    }
}

/// Вход этого приложения. Пока приложение запускается и ещё не создало [`Lock`], команда получает
/// понятный отказ, а не падает.
fn lock_of<R: Runtime>(app: &AppHandle<R>) -> Res<Lock> {
    app.try_state::<Lock>().map(|l| l.inner().clone()).ok_or_else(|| STARTING.to_string())
}

async fn reply<R: Runtime>(app: &AppHandle<R>, error: Option<String>) -> Res<LockStatus> {
    let lock = lock_of(app)?;
    let bio = bio_info(app, &lock.0.names).await;
    lock.status(&bio, error)
}

/// Работа с PIN-кодом в фоновом потоке: Argon2 не должен держать главный поток.
async fn blocking<R: Runtime, T: Send + 'static>(app: &AppHandle<R>, f: impl FnOnce(&Lock) -> Res<T> + Send + 'static) -> Res<T> {
    let lock = lock_of(app)?;
    tauri::async_runtime::spawn_blocking(move || f(&lock)).await.map_err(err)?
}

#[tauri::command]
pub async fn lock_status<R: Runtime>(app: AppHandle<R>) -> Res<LockStatus> {
    reply(&app, None).await
}

#[tauri::command]
pub async fn pin_unlock<R: Runtime>(app: AppHandle<R>, code: String) -> Res<LockStatus> {
    let error = blocking(&app, move |lock| lock.pin_unlock(&code)).await?;
    reply(&app, error).await
}

/// Новый PIN. Если PIN уже есть, нужен текущий — или подтверждение устройством («Забыли PIN?»).
#[tauri::command]
pub async fn pin_set<R: Runtime>(app: AppHandle<R>, code: String, current: Option<String>) -> Res<LockStatus> {
    if !pin::valid(&code) {
        return Err(format!("PIN-код — {} цифры", pin::PIN_LEN));
    }
    let error = blocking(&app, move |lock| lock.pin_set(&code, current.as_deref())).await?;
    reply(&app, error).await
}

/// Выключает вход по PIN-коду (и отпечатку). Нужен текущий PIN.
#[tauri::command]
pub async fn pin_off<R: Runtime>(app: AppHandle<R>, current: Option<String>) -> Res<LockStatus> {
    let error = blocking(&app, move |lock| lock.pin_off(current.as_deref())).await?;
    reply(&app, error).await
}

#[tauri::command]
pub async fn bio_unlock<R: Runtime>(app: AppHandle<R>) -> Res<LockStatus> {
    let lock = lock_of(&app)?;
    if !lock.meta_is(pin::META_BIO, "1") {
        return reply(&app, Some("Вход по отпечатку выключен".into())).await;
    }
    let names = lock.0.names;
    let error = match system_auth(&app, &names, &format!("Вход в «{}»", names.name_acc), false).await {
        Ok(true) => {
            lock.reset_fails()?;
            lock.unlock()?;
            None
        }
        Ok(false) => None,
        Err(e) => Some(e),
    };
    reply(&app, error).await
}

/// Включает или выключает вход по отпечатку. Включение сразу проверяет отпечаток.
#[tauri::command]
pub async fn bio_set<R: Runtime>(app: AppHandle<R>, on: bool) -> Res<LockStatus> {
    let lock = lock_of(&app)?;
    {
        let g = lock.state()?;
        if g.locked {
            return Err(LOCKED.into());
        }
        if on && !g.pin {
            return Err("Сначала задайте PIN-код".into());
        }
    }
    let mut error = None;
    let allowed = if on {
        match system_auth(&app, &lock.0.names, "Включить вход по отпечатку", false).await {
            Ok(ok) => ok,
            Err(e) => {
                error = Some(e);
                false
            }
        }
    } else {
        true
    };
    if allowed {
        lock.0.meta.set(pin::META_BIO, if on { "1" } else { "0" })?;
    }
    reply(&app, error).await
}

/// «Забыли PIN?»: проверка блокировкой устройства, после неё можно задать новый PIN.
#[tauri::command]
pub async fn pin_recover<R: Runtime>(app: AppHandle<R>) -> Res<LockStatus> {
    let lock = lock_of(&app)?;
    let names = lock.0.names;
    let error = match system_auth(&app, &names, &format!("Сброс PIN-кода «{}»", names.name_gen), true).await {
        Ok(true) => {
            lock.recovered()?;
            None
        }
        Ok(false) => None,
        Err(e) => Some(e),
    };
    reply(&app, error).await
}

#[tauri::command]
pub async fn lock_now<R: Runtime>(app: AppHandle<R>) -> Res<LockStatus> {
    lock_of(&app)?.lock_now()?;
    reply(&app, None).await
}

/// Экран сообщает, что приложение свернули или снова открыли (в дополнение к фокусу окна).
#[tauri::command]
pub async fn app_visible<R: Runtime>(app: AppHandle<R>, visible: bool) -> Res<LockStatus> {
    on_focus(&app, visible);
    reply(&app, None).await
}

/// Окно потеряло или получило фокус: через 5 минут в фоне приложение блокируется.
/// Приложение вызывает это из `on_window_event` на `WindowEvent::Focused`.
pub fn on_focus<R: Runtime>(app: &AppHandle<R>, focused: bool) {
    let Ok(lock) = lock_of(app) else { return };
    if lock.focus_changed(focused, lock.lock_after()) {
        let _ = app.emit(LOCKED_EVENT, ());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const APP: AppNames = AppNames { id: "kittest", name: "Проба", name_acc: "Пробу", name_gen: "Пробы", feminine: true };

    /// Местные настройки в памяти; копии смотрят в одно место.
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

    struct Side {
        lock: Lock,
        meta: MemMeta,
        locks: Arc<AtomicUsize>,
        resumes: Arc<AtomicUsize>,
    }

    fn side(meta: MemMeta) -> Side {
        let (locks, resumes) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (l, r) = (locks.clone(), resumes.clone());
        let count = |n: Arc<AtomicUsize>| -> Box<dyn Fn() + Send + Sync> {
            Box::new(move || {
                n.fetch_add(1, Ordering::SeqCst);
            })
        };
        let hooks = Hooks { on_lock: count(l), on_resume: count(r) };
        Side { lock: Lock::new(APP, meta.clone(), hooks), meta, locks, resumes }
    }

    fn with_pin(code: &str) -> Side {
        let meta = MemMeta::default();
        meta.set(pin::META_HASH, &pin::hash(code).unwrap()).unwrap();
        side(meta)
    }

    #[test]
    fn without_pin_the_app_is_open() {
        let s = side(MemMeta::default());
        assert!(!s.lock.locked());
        assert!(!s.lock.has_pin());
        assert!(s.lock.check().is_ok());
        // Без PIN-кода блокировать нечем.
        s.lock.lock_now().unwrap();
        assert!(!s.lock.locked());
        assert_eq!(s.locks.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn with_pin_the_app_opens_locked_and_unlocks_by_pin() {
        let s = with_pin("2580");
        assert!(s.lock.locked());
        assert_eq!(s.lock.check(), Err(LOCKED.to_string()));
        assert_eq!(s.lock.pin_unlock("0000").unwrap().as_deref(), Some("Неверный PIN-код"));
        assert!(s.lock.locked());
        assert_eq!(s.meta.get(pin::META_FAILS).unwrap().as_deref(), Some("1"));
        assert_eq!(s.lock.pin_unlock("2580").unwrap(), None);
        assert!(!s.lock.locked());
        assert_eq!(s.meta.get(pin::META_FAILS).unwrap().as_deref(), Some("0"), "верный PIN сбрасывает счётчик ошибок");
    }

    #[test]
    fn wrong_pins_end_in_a_pause_even_for_the_right_pin() {
        let s = with_pin("2580");
        for _ in 0..pin::FREE_TRIES {
            assert_eq!(s.lock.pin_unlock("1111").unwrap().as_deref(), Some("Неверный PIN-код"));
        }
        let until: u64 = s.meta.get(pin::META_UNTIL).unwrap().unwrap().parse().unwrap();
        assert!(until > now_secs(), "после пятой ошибки — пауза");
        assert_eq!(s.lock.pin_unlock("2580").unwrap().as_deref(), Some("Слишком много попыток, подождите"));
        assert!(s.lock.locked());
        // Пауза прошла — верный PIN открывает.
        s.meta.set(pin::META_UNTIL, "1").unwrap();
        assert_eq!(s.lock.pin_unlock("2580").unwrap(), None);
        assert!(!s.lock.locked());
    }

    #[test]
    fn pin_is_set_changed_and_turned_off_with_the_current_pin() {
        let s = side(MemMeta::default());
        assert_eq!(s.lock.pin_set("2580", None).unwrap(), None);
        assert!(s.lock.has_pin() && !s.lock.locked());
        assert!(pin::verify("2580", &s.meta.get(pin::META_HASH).unwrap().unwrap()));

        assert_eq!(s.lock.pin_set("1357", None).unwrap().as_deref(), Some("Введите текущий PIN-код"));
        assert_eq!(s.lock.pin_set("1357", Some("0000")).unwrap().as_deref(), Some("Неверный PIN-код"));
        assert_eq!(s.lock.pin_set("1357", Some("2580")).unwrap(), None);
        assert!(pin::verify("1357", &s.meta.get(pin::META_HASH).unwrap().unwrap()));

        s.meta.set(pin::META_BIO, "1").unwrap();
        assert_eq!(s.lock.pin_off(None).unwrap().as_deref(), Some("Введите текущий PIN-код"));
        assert_eq!(s.lock.pin_off(Some("2580")).unwrap().as_deref(), Some("Неверный PIN-код"));
        assert_eq!(s.lock.pin_off(Some("1357")).unwrap(), None);
        assert!(!s.lock.has_pin());
        assert_eq!(s.meta.get(pin::META_HASH).unwrap().as_deref(), Some(""));
        assert_eq!(s.meta.get(pin::META_BIO).unwrap().as_deref(), Some("0"), "без PIN-кода нет и входа по отпечатку");
        // Приложение, запущенное после этого, открывается без входа.
        assert!(!side(s.meta.clone()).lock.locked());
    }

    #[test]
    fn forgotten_pin_is_replaced_after_the_device_confirms_the_owner() {
        let s = with_pin("2580");
        assert_eq!(s.lock.pin_set("1357", None).unwrap().as_deref(), Some("Введите текущий PIN-код"));
        s.meta.set(pin::META_FAILS, "7").unwrap();
        s.lock.recovered().unwrap();
        assert!(!s.lock.locked());
        assert_eq!(s.meta.get(pin::META_FAILS).unwrap().as_deref(), Some("0"));
        assert_eq!(s.lock.pin_set("1357", None).unwrap(), None, "после подтверждения старый PIN не нужен");
        // Подтверждение разовое: следующая смена снова требует текущий PIN.
        assert_eq!(s.lock.pin_set("2468", None).unwrap().as_deref(), Some("Введите текущий PIN-код"));
    }

    #[test]
    fn lock_now_locks_and_tells_the_app() {
        let s = with_pin("2580");
        s.lock.pin_unlock("2580").unwrap();
        s.lock.lock_now().unwrap();
        assert!(s.lock.locked());
        assert_eq!(s.locks.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn long_time_in_background_locks_the_app() {
        let s = with_pin("2580");
        s.lock.pin_unlock("2580").unwrap();
        // Ненадолго свернули — вход не спрашиваем.
        assert!(!s.lock.focus_changed(false, Duration::from_secs(300)));
        assert!(!s.lock.focus_changed(true, Duration::from_secs(300)));
        assert!(!s.lock.locked());
        assert_eq!((s.resumes.load(Ordering::SeqCst), s.locks.load(Ordering::SeqCst)), (1, 0));
        // Были в фоне дольше срока — блокировка.
        assert!(!s.lock.focus_changed(false, Duration::ZERO));
        assert!(s.lock.focus_changed(true, Duration::ZERO));
        assert!(s.lock.locked());
        assert_eq!((s.resumes.load(Ordering::SeqCst), s.locks.load(Ordering::SeqCst)), (2, 1));
        // Уже заблокировано — второй раз не блокируем.
        s.lock.focus_changed(false, Duration::ZERO);
        assert!(!s.lock.focus_changed(true, Duration::ZERO));
    }

    #[test]
    fn without_pin_background_never_locks() {
        let s = side(MemMeta::default());
        s.lock.focus_changed(false, Duration::ZERO);
        assert!(!s.lock.focus_changed(true, Duration::ZERO));
        assert!(!s.lock.locked());
    }

    #[test]
    fn notice_is_shown_once() {
        let s = side(MemMeta::default());
        s.lock.set_notice(Some("Прежнюю базу не удалось открыть".into()));
        let bio = Bio { ready: false, kind: "", recover: "" };
        assert_eq!(s.lock.status(&bio, None).unwrap().notice.as_deref(), Some("Прежнюю базу не удалось открыть"));
        assert_eq!(s.lock.status(&bio, None).unwrap().notice, None);
    }

    /// Экран или окно обратились раньше, чем приложение создало вход: отказ, а не падение.
    #[test]
    fn before_the_app_is_ready_commands_refuse_politely() {
        let app = tauri::test::mock_builder().build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
        let h = app.handle().clone();
        on_focus(&h, true);
        tauri::async_runtime::block_on(async {
            assert_eq!(lock_status(h.clone()).await.unwrap_err(), STARTING);
            assert_eq!(pin_unlock(h.clone(), "2580".into()).await.unwrap_err(), STARTING);
            assert_eq!(lock_now(h.clone()).await.unwrap_err(), STARTING);
        });
    }

    /// Команды через Tauri: то, что вызывает экран.
    #[test]
    fn commands_work_in_a_tauri_app() {
        let s = with_pin("2580");
        let app = tauri::test::mock_builder().manage(s.lock.clone()).build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
        let h = app.handle().clone();
        tauri::async_runtime::block_on(async {
            let st = lock_status(h.clone()).await.unwrap();
            assert!(st.pin && st.locked && !st.bio);
            assert_eq!((st.left, st.wait, st.key), (pin::FREE_TRIES, 0, kit_secure::method()));

            let st = pin_unlock(h.clone(), "0000".into()).await.unwrap();
            assert!(st.locked);
            assert_eq!(st.error.as_deref(), Some("Неверный PIN-код"));
            assert_eq!(st.left, pin::FREE_TRIES - 1);

            let st = pin_unlock(h.clone(), "2580".into()).await.unwrap();
            assert!(!st.locked && st.error.is_none());

            assert_eq!(pin_set(h.clone(), "12".into(), None).await.unwrap_err(), "PIN-код — 4 цифры");
            let st = pin_set(h.clone(), "1357".into(), Some("2580".into())).await.unwrap();
            assert!(st.pin && st.error.is_none());

            let st = lock_now(h.clone()).await.unwrap();
            assert!(st.locked);
            assert_eq!(bio_set(h.clone(), true).await.unwrap_err(), LOCKED, "пока заблокировано, вход по отпечатку не включить");
            let st = bio_unlock(h.clone()).await.unwrap();
            assert!(st.locked);
            assert_eq!(st.error.as_deref(), Some("Вход по отпечатку выключен"));

            let st = pin_unlock(h.clone(), "1357".into()).await.unwrap();
            assert!(!st.locked);
            let st = pin_off(h.clone(), Some("1357".into())).await.unwrap();
            assert!(!st.pin && !st.locked);
        });
    }
}
