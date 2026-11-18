//! Облако в приложении: вход, пароль копий, обмен в фоне и команды экрана. Одинаково у всех
//! приложений на модуле; своё у приложения — формат облака (`kit_core::cloud::Format`), база, строки
//! ключей и, если нужно, другие способы входа ([`Extra`]: у «Кармана» — Яндекс Диск и Google Drive по коду).
//!
//! Пароль приложения Яндекса (или пароль своего сервера) хранится только на этом устройстве:
//! в зашифрованной базе и ещё под защитой системы. В копии и на другие устройства он не уходит.
//!
//! Обмен идёт сам: вскоре после запуска, через минуту после изменений, каждые 15 минут
//! и при возвращении в приложение, если с прошлого обмена прошло больше двух минут.
//! Настройка «Только по кнопке» оставляет одну кнопку «Обменяться сейчас».
//!
//! Пароль облака — это пароль копий: на компьютере они совпадают, смена пароля копий
//! перешифровывает ключ облака. Телефон пароль копий не хранит — его вводят один раз при подключении.
//!
//! Подключение: приложение кладёт в состояние [`Sky`] (`app.manage(Sky::new(…))`), запускает фон
//! [`start`], зовёт [`resumed`] при возвращении в приложение и объявляет команды экрана макросом
//! [`cloud_commands!`](crate::cloud_commands) — их имена одинаковы у всех приложений (`cloud_status`, …).

use crate::guard::Lock;
use crate::{err, Res};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use kit_cloud::dav::{self, Dav};
use kit_core::backup::{self, BackupKey, Kdf};
use kit_core::cloud::{self as kc, Cloud, CloudFail, CloudKey, CloudStore, Dir, Entry, KeyFile, Report, Round, State as CloudState, Step};
use kit_core::lan::Device;
use kit_secure::KeyNames;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};

const META_PROVIDER: &str = "cloud.provider";
const META_DAV: &str = "cloud.dav";
const META_KEY: &str = "cloud.key";
const META_STATE: &str = "cloud.state";
const META_EVERY: &str = "cloud.every";
const META_LAST: &str = "cloud.last";
const META_ERROR: &str = "cloud.error";
/// Новый пароль копий: ключ облака, перешифрованный им, ещё не ушёл в облако.
const META_KEYFILE: &str = "cloud.keyfile";
/// В облаке уже есть данные приложения: пароль копий вводят, а не придумывают.
const META_EXISTS: &str = "cloud.exists";
/// Компьютер, а не телефон: на компьютере пароль облака — это и пароль копий.
const DESKTOP: bool = !cfg!(any(target_os = "android", target_os = "ios"));
const EVERY: Duration = Duration::from_secs(15 * 60);
const RESUME_GAP: Duration = Duration::from_secs(2 * 60);
const RETRY: Duration = Duration::from_secs(5 * 60);

static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);
static MANUAL: AtomicBool = AtomicBool::new(false);
static RESUMED: AtomicBool = AtomicBool::new(false);
static PROGRESS: Mutex<Option<Step>> = Mutex::new(None);
/// Один обмен за раз: фон, кнопка и отключение облака ждут друг друга.
static ROUND: Mutex<()> = Mutex::new(());

struct Wake {
    flag: Mutex<bool>,
    cv: Condvar,
}

static WAKE: Wake = Wake { flag: Mutex::new(false), cv: Condvar::new() };

fn wake() {
    if let Ok(mut f) = WAKE.flag.lock() {
        *f = true;
    }
    WAKE.cv.notify_all();
}

fn sleep_or_wake(d: Duration) {
    let Ok(g) = WAKE.flag.lock() else {
        std::thread::sleep(d);
        return;
    };
    if let Ok((mut g, _)) = WAKE.cv.wait_timeout_while(g, d, |woken| !*woken) {
        *g = false;
    }
}

/// Обменяться при первой возможности: по кнопке, после нового пароля копий.
pub fn sync_now() {
    MANUAL.store(true, Ordering::SeqCst);
    wake();
}

/// Вернулись в приложение: обменяться, если с прошлого обмена прошло больше двух минут.
pub fn resumed() {
    RESUMED.store(true, Ordering::SeqCst);
    wake();
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn meta<J: Device>(st: &J, k: &str) -> Option<String> {
    st.local(k).filter(|v| !v.is_empty())
}

fn set<J: Device>(st: &J, k: &str, v: &str) -> Res<()> {
    st.set_local(k, v).map_err(err)
}

fn clear<J: Device>(st: &J, keys: &[&str]) {
    for k in keys {
        let _ = st.set_local(k, "");
    }
}

// ——— Настройки приложения и другие способы входа ———

/// Облако одного приложения в приложении: формат, строки ключей и где что лежит в базе.
pub struct Settings {
    /// Формат облака: папка, файл ключа, метки.
    pub cloud: &'static kc::Format,
    /// Формат резервной копии: для проверки нового пароля копий.
    pub backup: &'static backup::Format,
    /// Строки ключей приложения для хранилища системы: пароль сервера — под их защитой.
    pub keys: &'static KeyNames,
    /// Местная настройка с паролем сервера (под защитой системы) или входом по коду: у «Полки» —
    /// `cloud.password`, у «Кармана» — `cloud.tokens`. Менять нельзя: вход на устройствах пропадёт.
    pub secret: &'static str,
    /// Местная настройка с ключом резервных копий (`backup.key`).
    pub backup_key: &'static str,
}

/// Подключение к облаку на один обмен.
pub trait Conn: Cloud + Send {
    /// После удаления данных: убрать пустые папки приложения.
    fn tidy(&self) -> kit_core::Result<()> {
        Ok(())
    }
    /// После обмена: что сохранить на устройстве (вход по коду обновил ключи доступа).
    fn renewed(&self) -> Option<String> {
        None
    }
}

impl Conn for Dav {
    fn tidy(&self) -> kit_core::Result<()> {
        Dav::tidy(self)
    }
}

/// Способ подключения для экрана: `password` — логин и пароль, `code` — вход по коду в браузере.
#[derive(Clone, Serialize)]
pub struct ProviderView {
    pub code: &'static str,
    pub title: &'static str,
    pub kind: &'static str,
}

/// Другие способы входа, кроме логина и пароля WebDAV (у «Кармана» — по коду). Их команды
/// приложение пишет само; здесь — как открыть облако по сохранённому входу и что показать на экране.
pub trait Extra: Send + Sync + 'static {
    /// Способы входа для экрана (только свои, «по коду»).
    fn providers(&self) -> Vec<ProviderView>;
    /// Название облака по коду способа: `yandex` → «Яндекс Диск».
    fn title(&self, link: &str) -> Option<&'static str>;
    /// Облако по сохранённому входу (`secret` — содержимое местной настройки [`Settings::secret`]).
    /// `None` — способ не этот.
    fn open(&self, link: &str, secret: &str) -> Option<Res<Box<dyn Conn>>>;
    /// Вход, который ждёт ввода кода, — для экрана; `None` — не ждём.
    fn login(&self) -> Option<serde_json::Value> {
        None
    }
}

/// Облако в приложении: настройки, база и другие способы входа.
pub struct Sky<J> {
    pub cfg: Settings,
    store: Arc<Mutex<J>>,
    extra: Option<Box<dyn Extra>>,
}

impl<J: CloudStore + Device> Sky<J> {
    pub fn new(cfg: Settings, store: Arc<Mutex<J>>, extra: Option<Box<dyn Extra>>) -> Sky<J> {
        Sky { cfg, store, extra }
    }

    fn names(&self) -> kit_core::AppNames {
        self.cfg.cloud.names
    }

    fn st(&self) -> MutexGuard<'_, J> {
        lock(&self.store)
    }
}

fn sky<R: Runtime, J: CloudStore + Device>(app: &AppHandle<R>) -> tauri::State<'_, Sky<J>> {
    app.state::<Sky<J>>()
}

fn unlocked<R: Runtime>(app: &AppHandle<R>) -> Res<()> {
    match app.try_state::<Lock>() {
        Some(l) => l.check(),
        None => Ok(()),
    }
}

// ——— Как подключено облако ———

/// Вход по логину и паролю: Яндекс Диск (пароль приложения) или свой сервер WebDAV.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Link {
    YaDisk,
    WebDav,
}

impl Link {
    fn code(self) -> &'static str {
        match self {
            Link::YaDisk => "yadisk",
            Link::WebDav => "webdav",
        }
    }

    fn parse(s: &str) -> Option<Link> {
        match s {
            "yadisk" => Some(Link::YaDisk),
            "webdav" => Some(Link::WebDav),
            _ => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Link::YaDisk => "Яндекс Диск",
            Link::WebDav => "Сервер WebDAV",
        }
    }

    /// Надпись на кнопке подключения.
    fn button(self) -> &'static str {
        match self {
            Link::YaDisk => "Яндекс Диск",
            Link::WebDav => "Другой сервер WebDAV",
        }
    }
}

/// Сервер WebDAV и логин (пароль хранится отдельно).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DavLogin {
    url: String,
    user: String,
}

fn provider_of<J: Device>(st: &J) -> Option<String> {
    meta(st, META_PROVIDER)
}

fn dav_of<J: Device>(st: &J) -> Option<DavLogin> {
    meta(st, META_DAV).and_then(|t| serde_json::from_str(&t).ok())
}

/// Пароль сервера из базы: он лежит там под защитой системы.
fn dav_password<J: CloudStore + Device>(sky: &Sky<J>, st: &J) -> Res<Option<String>> {
    let Some(text) = meta(st, sky.cfg.secret) else { return Ok(None) };
    let blob = B64.decode(text.trim()).map_err(err)?;
    let plain = kit_secure::unprotect(sky.cfg.keys, &blob).map_err(|e| format!("пароль облака не достать из хранилища системы: {e}"))?;
    String::from_utf8(plain).map(Some).map_err(err)
}

/// Для сквозных проверок в отладочной сборке: имитация облака на этом компьютере (`<ID>_CLOUD_MOCK`).
#[cfg(debug_assertions)]
pub fn mock_base(names: kit_core::AppNames) -> Option<String> {
    std::env::var(names.env("CLOUD_MOCK")).ok().filter(|b| b.starts_with("http://127.0.0.1:"))
}

/// Адрес Яндекс Диска по WebDAV; в сквозных проверках — имитация.
fn yadisk_url(names: kit_core::AppNames) -> String {
    #[cfg(debug_assertions)]
    if let Some(base) = mock_base(names) {
        return format!("{base}/dav");
    }
    let _ = names;
    dav::YANDEX.into()
}

/// Подключение по входу этого устройства.
fn open<J: CloudStore + Device>(sky: &Sky<J>, st: &J) -> Res<Box<dyn Conn>> {
    let link = provider_of(st).ok_or("Сначала войдите в облако")?;
    if Link::parse(&link).is_some() {
        let login = dav_of(st).ok_or("Сначала войдите в облако")?;
        let password = dav_password(sky, st)?.ok_or("Сначала войдите в облако")?;
        return Ok(Box::new(Dav::new(sky.cfg.cloud, &login.url, &login.user, &password)));
    }
    let secret = meta(st, sky.cfg.secret).ok_or("Сначала войдите в облако")?;
    sky.extra.as_ref().and_then(|x| x.open(&link, &secret)).unwrap_or_else(|| Err("Сначала войдите в облако".into()))
}

fn title<J: CloudStore + Device>(sky: &Sky<J>, link: &str) -> Option<&'static str> {
    Link::parse(link).map(Link::title).or_else(|| sky.extra.as_ref().and_then(|x| x.title(link)))
}

/// Страницу открываем, только если она у Яндекса.
fn safe_page(url: &str) -> bool {
    const PAGES: [&str; 2] = ["https://id.yandex.ru/", "https://yandex.ru/support/"];
    PAGES.iter().any(|p| url.starts_with(p)) && !url.chars().any(|c| c.is_whitespace() || c == '"')
}

fn open_page<R: Runtime>(app: &AppHandle<R>, url: &str) {
    if !safe_page(url) {
        return;
    }
    use tauri_plugin_opener::OpenerExt;
    if let Err(e) = app.opener().open_url(url, None::<&str>) {
        eprintln!("страница не открылась: {e}");
    }
}

// ——— Состояние для экрана ———

/// Вход по паролю: сервер и логин (пароль экран не получает).
#[derive(Serialize)]
pub struct DavView {
    url: String,
    host: String,
    user: String,
}

/// Последний обмен.
#[derive(Clone, Serialize, Deserialize)]
pub struct Last {
    pub at: i64,
    pub received: usize,
    pub sent: usize,
    pub files_received: usize,
    pub files_sent: usize,
    pub files_waiting: usize,
    /// Большие файлы (у «Полки» — файлы книг): пришло, ушло, ещё не в облаке целиком.
    #[serde(default, alias = "books_received")]
    pub whole_received: usize,
    #[serde(default, alias = "books_sent")]
    pub whole_sent: usize,
    #[serde(default, alias = "books_waiting")]
    pub whole_waiting: usize,
    #[serde(default)]
    pub problems: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Problem {
    pub kind: CloudFail,
    pub text: String,
    pub at: i64,
}

#[derive(Serialize)]
pub struct Status {
    desktop: bool,
    /// Чем можно подключиться: Яндекс Диск и свой сервер — всегда; другие способы — если есть у приложения.
    providers: Vec<ProviderView>,
    /// off — не подключено; signin — ждём, пока введут код; password — нужен пароль копий;
    /// relogin — вход устарел или пароль сервера не подходит; ready — подключено.
    state: &'static str,
    provider: Option<String>,
    title: Option<&'static str>,
    /// Вход по коду, который ждёт ввода.
    login: Option<serde_json::Value>,
    dav: Option<DavView>,
    /// В облаке уже есть данные приложения: пароль вводят, а не придумывают.
    cloud_exists: bool,
    /// На этом устройстве задан пароль копий.
    backup_password: bool,
    /// auto — обмен сам, manual — только по кнопке.
    every: String,
    running: bool,
    progress: Option<Step>,
    last: Option<Last>,
    error: Option<Problem>,
    /// Новый пароль копий ещё не ушёл в облако.
    key_pending: bool,
    /// Сообщение после действия.
    notice: Option<String>,
}

fn every<J: Device>(st: &J) -> String {
    if meta(st, META_EVERY).as_deref() == Some("manual") {
        "manual".into()
    } else {
        "auto".into()
    }
}

fn status_of<J: CloudStore + Device>(sky: &Sky<J>, st: &J, notice: Option<String>) -> Status {
    let provider = provider_of(st);
    let login = sky.extra.as_ref().and_then(|x| x.login());
    let (secret, key) = (meta(st, sky.cfg.secret).is_some(), meta(st, META_KEY).is_some());
    let name = match (login.is_some(), provider.is_some(), secret, key) {
        (true, ..) => "signin",
        (_, false, ..) => "off",
        (_, true, true, true) => "ready",
        (_, true, true, false) => "password",
        (_, true, false, true) => "relogin",
        (_, true, false, false) => "off",
    };
    let mut providers: Vec<ProviderView> = [Link::YaDisk, Link::WebDav].into_iter().map(|l| ProviderView { code: l.code(), title: l.button(), kind: "password" }).collect();
    providers.extend(sky.extra.as_ref().map(|x| x.providers()).unwrap_or_default());
    let by_password = provider.as_deref().and_then(Link::parse).is_some();
    Status {
        desktop: DESKTOP,
        providers,
        state: name,
        title: provider.as_deref().and_then(|p| title(sky, p)),
        provider,
        login,
        dav: by_password.then(|| dav_of(st)).flatten().map(|l| DavView { host: dav::host_of(&l.url).to_string(), url: l.url, user: l.user }),
        cloud_exists: meta(st, META_EXISTS).as_deref() == Some("1"),
        backup_password: meta(st, sky.cfg.backup_key).is_some(),
        every: every(st),
        running: RUNNING.load(Ordering::SeqCst),
        progress: PROGRESS.lock().ok().and_then(|p| *p),
        last: meta(st, META_LAST).and_then(|s| serde_json::from_str(&s).ok()),
        error: meta(st, META_ERROR).and_then(|s| serde_json::from_str(&s).ok()),
        key_pending: meta(st, META_KEYFILE).is_some(),
        notice,
    }
}

/// Облако для экрана.
pub fn status<R: Runtime, J: CloudStore + Device>(app: &AppHandle<R>, notice: Option<String>) -> Res<Status> {
    let sky = sky::<R, J>(app);
    let st = sky.st();
    Ok(status_of(&sky, &st, notice))
}

// ——— Команды ———

/// Страница Яндекс ID, где создают пароль приложения.
pub fn app_passwords<R: Runtime>(app: &AppHandle<R>) -> Res<()> {
    open_page(app, "https://id.yandex.ru/security");
    Ok(())
}

/// Вход по логину и паролю: Яндекс Диск (пароль приложения) или свой сервер WebDAV.
/// Проверяет вход, создаёт папку приложения и узнаёт, есть ли в ней данные.
pub async fn dav_login<R: Runtime, J: CloudStore + Device>(app: AppHandle<R>, kind: String, url: Option<String>, user: String, password: String) -> Res<Status> {
    unlocked(&app)?;
    let link = Link::parse(&kind).ok_or("Неизвестное облако")?;
    let mut bytes = password.into_bytes();
    tauri::async_runtime::spawn_blocking(move || {
        let r = std::str::from_utf8(&bytes).map_err(err).and_then(|p| dav_login_now::<R, J>(&app, link, url.as_deref(), &user, p));
        bytes.fill(0);
        std::hint::black_box(&bytes);
        r
    })
    .await
    .map_err(err)?
}

fn dav_login_now<R: Runtime, J: CloudStore + Device>(app: &AppHandle<R>, link: Link, url: Option<&str>, user: &str, password: &str) -> Res<Status> {
    let sky = sky::<R, J>(app);
    let user = user.trim();
    // Пароль приложения Яндекса — 16 латинских букв; пробелы при вставке убираем.
    let password: String = if link == Link::YaDisk { password.chars().filter(|c| !c.is_whitespace()).collect() } else { password.to_string() };
    if user.is_empty() {
        return Err(if link == Link::YaDisk { "Введите логин Яндекса" } else { "Введите логин" }.into());
    }
    if password.is_empty() {
        return Err(if link == Link::YaDisk { "Введите пароль приложения" } else { "Введите пароль" }.into());
    }
    let url = match link {
        Link::YaDisk => yadisk_url(sky.names()),
        Link::WebDav => dav::server_url(url.unwrap_or(""), cfg!(debug_assertions))?,
    };
    let exists = Dav::new(sky.cfg.cloud, &url, user, &password).connect().map_err(err)?;
    let blob = kit_secure::protect(sky.cfg.keys, password.as_bytes()).map_err(|e| format!("пароль не сохранить в хранилище системы: {e}"))?;
    let login = DavLogin { url, user: user.to_string() };
    let st = sky.st();
    // То же облако, что было (пароль сменили): обмен продолжается без пароля копий.
    let same = provider_of(&*st).as_deref() == Some(link.code()) && dav_of(&*st).as_ref() == Some(&login) && meta(&*st, META_KEY).is_some();
    if !same {
        clear(&*st, &[META_KEY, META_STATE, META_LAST, META_KEYFILE]);
    }
    set(&*st, META_PROVIDER, link.code())?;
    set(&*st, META_DAV, &serde_json::to_string(&login).map_err(err)?)?;
    set(&*st, sky.cfg.secret, &B64.encode(blob))?;
    set(&*st, META_EXISTS, if exists || same { "1" } else { "0" })?;
    set(&*st, META_ERROR, "")?;
    let s = status_of(&sky, &st, same.then(|| "Вход обновлён — обмен продолжается".to_string()));
    drop(st);
    if same {
        sync_now();
    }
    Ok(s)
}

/// Вход другим способом прошёл (у «Кармана» — по коду): запомнить способ и вход. Если это то же облако,
/// что было (вход устарел), обмен продолжается без пароля копий. `exists` — есть ли в облаке данные
/// приложения (спрашивают, только если облако новое).
pub fn connected<R: Runtime, J: CloudStore + Device>(app: &AppHandle<R>, link: &str, secret: &str, exists: impl FnOnce() -> Res<bool>) -> Res<()> {
    let sky = sky::<R, J>(app);
    let same = {
        let st = sky.st();
        provider_of(&*st).as_deref() == Some(link) && meta(&*st, META_KEY).is_some()
    };
    let exists = same || exists()?;
    let st = sky.st();
    if !same {
        clear(&*st, &[META_KEY, META_STATE, META_LAST, META_KEYFILE, META_DAV]);
    }
    set(&*st, META_PROVIDER, link)?;
    set(&*st, sky.cfg.secret, secret)?;
    set(&*st, META_EXISTS, if exists { "1" } else { "0" })?;
    set(&*st, META_ERROR, "")?;
    drop(st);
    if same {
        sync_now();
    }
    Ok(())
}

/// Пароль копий: открывает ключ облака, которое уже есть, или создаёт новое.
pub async fn password<R: Runtime, J: CloudStore + Device>(app: AppHandle<R>, password: String) -> Res<Status> {
    unlocked(&app)?;
    let mut bytes = password.into_bytes();
    tauri::async_runtime::spawn_blocking(move || {
        let r = std::str::from_utf8(&bytes).map_err(err).and_then(|p| set_password::<R, J>(&app, p));
        bytes.fill(0);
        std::hint::black_box(&bytes);
        r
    })
    .await
    .map_err(err)?
}

fn set_password<R: Runtime, J: CloudStore + Device>(app: &AppHandle<R>, password: &str) -> Res<Status> {
    backup::check_password(password).map_err(err)?;
    let sky = sky::<R, J>(app);
    let fmt = sky.cfg.cloud;
    let (conn, backup_meta) = {
        let st = sky.st();
        (open(&sky, &st)?, meta(&*st, sky.cfg.backup_key))
    };
    let existing = kc::read_key_file(fmt, &conn).map_err(err)?;
    // Новое облако — новый пароль на Диске: файлы там может скачать любой, кто войдёт в аккаунт,
    // поэтому самые простые пароли не годятся. Пароль облака, которое уже есть, не проверяем.
    if existing.is_none() {
        if let Err(e) = backup::check_new_password(sky.cfg.backup, password) {
            let hint = if DESKTOP && backup_meta.is_some() { " Смените пароль копий в разделе «Резервная копия» и подключите облако снова." } else { "" };
            return Err(format!("{e}{hint}"));
        }
    }
    // На компьютере пароль облака и пароль копий — один и тот же.
    let mut set_backup = false;
    let mut notice = None;
    if DESKTOP {
        match backup_meta.as_deref().map(BackupKey::from_meta).transpose().map_err(err)? {
            Some(bk) if bk.matches(password).map_err(err)? => {}
            Some(_) if existing.is_none() => {
                return Err("Это не пароль копий этого компьютера. Введите пароль, которым зашифрованы резервные копии, — сменить его можно в разделе «Резервная копия»".into());
            }
            Some(_) => {
                set_backup = true;
                notice = Some("Пароль копий на этом компьютере теперь тот же, что у облака. Новые копии делаются с ним, прежние открываются прежним паролем.".into());
            }
            None => {
                set_backup = true;
                notice = Some("Этот пароль теперь и пароль резервных копий на этом компьютере.".into());
            }
        }
    }
    let (key, _created) = kc::join(fmt, &conn, password, Kdf::DEFAULT, now()).map_err(err)?;
    let backup_key = if set_backup { Some(BackupKey::new(password).map_err(err)?) } else { None };
    let st = sky.st();
    set(&*st, META_KEY, &key.to_meta())?;
    clear(&*st, &[META_STATE, META_ERROR, META_KEYFILE]);
    set(&*st, META_EXISTS, "1")?;
    if let Some(bk) = backup_key {
        set(&*st, sky.cfg.backup_key, &bk.to_meta())?;
    }
    let s = status_of(&sky, &st, notice);
    drop(st);
    // Раздел копий показывает, задан ли пароль и зашифровано ли им облако.
    let _ = app.emit("backup", ());
    sync_now();
    Ok(s)
}

/// Обменяться сейчас.
pub fn sync_button<R: Runtime, J: CloudStore + Device>(app: &AppHandle<R>) -> Res<Status> {
    let sky = sky::<R, J>(app);
    let st = sky.st();
    if !ready(&sky, &*st) {
        return Err("Облако не подключено".into());
    }
    let s = status_of(&sky, &st, None);
    drop(st);
    sync_now();
    Ok(s)
}

/// Обмен сам или только по кнопке.
pub fn set_every<R: Runtime, J: CloudStore + Device>(app: &AppHandle<R>, every: String) -> Res<Status> {
    if !["auto", "manual"].contains(&every.as_str()) {
        return Err("Обмен: сам или только по кнопке".into());
    }
    set(&*sky::<R, J>(app).st(), META_EVERY, &every)?;
    status::<R, J>(app, None)
}

/// Отключает облако на этом устройстве; `wipe` — ещё и удаляет данные приложения из облака.
pub async fn disconnect<R: Runtime, J: CloudStore + Device>(app: AppHandle<R>, wipe: bool) -> Res<Status> {
    unlocked(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let sky = sky::<R, J>(&app);
        let names = sky.names();
        CANCEL.store(true, Ordering::SeqCst);
        // Ждём, пока идущий обмен остановится (он проверяет остановку между запросами).
        let _round = lock(&ROUND);
        let (loaded, yadisk) = {
            let st = sky.st();
            (open(&sky, &st), provider_of(&*st).as_deref() == Some(Link::YaDisk.code()))
        };
        let done = (|| -> Res<()> {
            let Ok(conn) = loaded else {
                return if wipe { Err("Чтобы удалить данные из облака, сначала войдите в него".into()) } else { Ok(()) };
            };
            if wipe {
                kc::wipe(sky.cfg.cloud, &conn, &AtomicBool::new(false)).map_err(err)?;
                // Пустые папки приложения убираем; не вышло — не страшно.
                let _ = conn.tidy();
            }
            Ok(())
        })();
        CANCEL.store(false, Ordering::SeqCst);
        // Данные из облака не удалились — остаёмся подключёнными.
        if wipe {
            done?;
        }
        let st = sky.st();
        clear(&*st, &[META_PROVIDER, sky.cfg.secret, META_DAV, META_KEY, META_STATE, META_LAST, META_ERROR, META_KEYFILE, META_EXISTS]);
        let mut note = if wipe {
            format!("Облако отключено, данные «{}» из него удалены.", names.name_gen)
        } else {
            format!("Облако отключено. Данные «{}» в нём остались — его можно подключить снова.", names.name_gen)
        };
        if yadisk {
            note.push_str(&format!(" Пароль приложения больше не нужен «{}» — его можно удалить в Яндекс ID.", names.name_acc));
        }
        let s = status_of(&sky, &st, Some(note));
        drop(st);
        let _ = app.emit("backup", ());
        Ok(s)
    })
    .await
    .map_err(err)?
}

/// Ключ облака на этом устройстве, если облако подключено.
pub fn key_meta<J: Device>(st: &J) -> Option<String> {
    meta(st, META_KEY)
}

/// Пароль копий сменили: тот же ключ облака под новым паролем (около секунды).
pub fn rewrap(fmt: &'static kc::Format, key_meta: &str, password: &str) -> Res<KeyFile> {
    let key = CloudKey::from_meta(fmt, key_meta).map_err(err)?;
    key.key_file(password, Kdf::DEFAULT, now()).map_err(err)
}

/// Новый ключ облака уйдёт в облако при ближайшем обмене.
pub fn save_keyfile<J: Device>(st: &J, kf: &KeyFile) -> Res<()> {
    set(st, META_KEYFILE, &String::from_utf8_lossy(&kf.to_bytes()))?;
    sync_now();
    Ok(())
}

// ——— Обмен в фоне ———

struct Loaded {
    conn: Res<Box<dyn Conn>>,
    key: CloudKey,
    state: CloudState,
    keyfile: Option<String>,
}

fn load<J: CloudStore + Device>(sky: &Sky<J>, st: &J) -> Option<Loaded> {
    if !ready(sky, st) {
        return None;
    }
    Some(Loaded {
        conn: open(sky, st),
        key: CloudKey::from_meta(sky.cfg.cloud, &meta(st, META_KEY)?).ok()?,
        state: meta(st, META_STATE).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
        keyfile: meta(st, META_KEYFILE),
    })
}

/// Событие для экрана: обмен идёт или закончился; сколько пришло нового.
#[derive(Clone, Serialize)]
struct CloudEvent {
    running: bool,
    received: usize,
    step: Option<Step>,
}

fn emit<R: Runtime>(app: &AppHandle<R>, ev: CloudEvent) {
    let _ = app.emit("cloud", ev);
}

struct RoundDone;

impl Drop for RoundDone {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
        if let Ok(mut p) = PROGRESS.lock() {
            *p = None;
        }
    }
}

/// Один обмен. Возвращает, что пошло не так.
fn run_round<R: Runtime, J: CloudStore + Device>(app: &AppHandle<R>) -> Option<CloudFail> {
    let Ok(_round) = ROUND.try_lock() else { return None };
    let sky = sky::<R, J>(app);
    let c = load(&sky, &*sky.st())?;
    RUNNING.store(true, Ordering::SeqCst);
    let _done = RoundDone;
    emit(app, CloudEvent { running: true, received: 0, step: None });
    let conn = match c.conn {
        Ok(s) => s,
        Err(e) => return fail::<R, J>(app, CloudFail::Auth, e),
    };
    let mut cstate = c.state;
    let mut last_emit = Instant::now();
    let mut result: kit_core::Result<Report> = Ok(Report::default());
    if let Some(kf_text) = &c.keyfile {
        match KeyFile::parse(sky.cfg.cloud, kf_text.as_bytes()).and_then(|kf| kc::put_key_file(&conn, &c.key, &kf)) {
            Ok(()) => {
                let st = sky.st();
                // Пока шёл обмен, пароль могли сменить ещё раз — тогда ждёт уже новый ключ.
                if meta(&*st, META_KEYFILE).as_deref() == Some(kf_text) {
                    clear(&*st, &[META_KEYFILE]);
                }
            }
            Err(e) => result = Err(e),
        }
    }
    if result.is_ok() {
        let mut progress = |step: Step| {
            if let Ok(mut p) = PROGRESS.lock() {
                *p = Some(step);
            }
            if last_emit.elapsed() >= Duration::from_millis(400) {
                last_emit = Instant::now();
                emit(app, CloudEvent { running: true, received: 0, step: Some(step) });
            }
        };
        result = kc::sync(&Round { cloud: &conn, store: &*sky.store, key: &c.key, now: now(), cancel: &CANCEL }, &mut cstate, &mut progress);
    }
    let st = sky.st();
    let _ = st.set_local(META_STATE, &serde_json::to_string(&cstate).unwrap_or_default());
    if let Some(secret) = conn.renewed() {
        let _ = st.set_local(sky.cfg.secret, &secret);
    }
    match result {
        Ok(r) => {
            let last = Last {
                at: now(),
                received: r.received,
                sent: r.sent,
                files_received: r.files_received,
                files_sent: r.files_sent,
                files_waiting: r.files_waiting,
                whole_received: r.whole_received,
                whole_sent: r.whole_sent,
                whole_waiting: r.whole_waiting,
                problems: r.problems,
            };
            let _ = st.set_local(META_LAST, &serde_json::to_string(&last).unwrap_or_default());
            clear(&*st, &[META_ERROR]);
            drop(st);
            emit(app, CloudEvent { running: false, received: r.received + r.files_received + r.whole_received, step: None });
            None
        }
        Err(e) => {
            drop(st);
            let (kind, text) = match e {
                kit_core::Error::Cloud(k, t) => (k, t),
                kit_core::Error::CloudPassword => (CloudFail::Replaced, e.to_string()),
                other => (CloudFail::Server, format!("Обмен с облаком не получился: {other}")),
            };
            if kind == CloudFail::Cancelled {
                emit(app, CloudEvent { running: false, received: 0, step: None });
                return None;
            }
            fail::<R, J>(app, kind, text)
        }
    }
}

/// Запоминает, что пошло не так, и что для этого нужно сделать.
fn fail<R: Runtime, J: CloudStore + Device>(app: &AppHandle<R>, kind: CloudFail, text: String) -> Option<CloudFail> {
    let sky = sky::<R, J>(app);
    {
        let st = sky.st();
        match kind {
            // Вход не подходит: войти снова — ключ облака остаётся, пароль копий не понадобится.
            CloudFail::Auth => clear(&*st, &[sky.cfg.secret]),
            // Данных в облаке нет или облако создано заново: нужен пароль копий.
            CloudFail::Gone => {
                clear(&*st, &[META_KEY, META_STATE, META_KEYFILE]);
                let _ = st.set_local(META_EXISTS, "0");
            }
            CloudFail::Replaced => {
                clear(&*st, &[META_KEY, META_STATE, META_KEYFILE]);
                let _ = st.set_local(META_EXISTS, "1");
            }
            _ => {}
        }
        let p = Problem { kind, text, at: now() };
        let _ = st.set_local(META_ERROR, &serde_json::to_string(&p).unwrap_or_default());
    }
    if matches!(kind, CloudFail::Gone | CloudFail::Replaced) {
        let _ = app.emit("backup", ());
    }
    emit(app, CloudEvent { running: false, received: 0, step: None });
    Some(kind)
}

fn ready<J: CloudStore + Device>(sky: &Sky<J>, st: &J) -> bool {
    provider_of(st).is_some() && meta(st, sky.cfg.secret).is_some() && meta(st, META_KEY).is_some()
}

/// Для сквозных проверок в отладочной сборке паузы короче (`<ID>_CLOUD_TICK_SECS` и т. д.).
fn secs(names: kit_core::AppNames, what: &str, default: u64) -> Duration {
    #[cfg(debug_assertions)]
    if let Some(s) = std::env::var(names.env(what)).ok().and_then(|v| v.parse().ok()) {
        return Duration::from_secs(s);
    }
    let _ = (names, what);
    Duration::from_secs(default)
}

/// Обмен в фоне: проверяем каждые полминуты, не пора ли.
pub fn start<R: Runtime, J: CloudStore + Device>(app: AppHandle<R>) {
    std::thread::spawn(move || {
        let names = sky::<R, J>(&app).names();
        let tick = secs(names, "CLOUD_TICK_SECS", 30);
        let change_wait = secs(names, "CLOUD_CHANGE_SECS", 60);
        // Первый обмен — вскоре после запуска, чтобы не мешать открытию.
        sleep_or_wake(secs(names, "CLOUD_DELAY_SECS", 10));
        let mut seen: Option<u64> = None;
        let mut changed_at: Option<Instant> = None;
        let mut last_round: Option<Instant> = None;
        let mut retry_at: Option<Instant> = None;
        let mut first = true;
        loop {
            if !first {
                sleep_or_wake(tick);
            }
            first = false;
            let sky = sky::<R, J>(&app);
            let (is_ready, auto, events) = {
                let st = sky.st();
                (ready(&sky, &*st), every(&*st) == "auto", st.event_count())
            };
            let manual = MANUAL.swap(false, Ordering::SeqCst);
            let back = RESUMED.swap(false, Ordering::SeqCst);
            if !is_ready {
                last_round = None;
                retry_at = None;
                continue;
            }
            if seen.is_some_and(|s| s != events) && changed_at.is_none() {
                changed_at = Some(Instant::now());
            }
            seen.get_or_insert(events);
            let since = last_round.map(|t| t.elapsed());
            let can_retry = retry_at.is_none_or(|t| Instant::now() >= t);
            let due = manual
                || (auto && can_retry && (since.is_none_or(|d| d >= EVERY) || changed_at.is_some_and(|t| t.elapsed() >= change_wait) || (back && since.is_none_or(|d| d >= RESUME_GAP))));
            if !due {
                continue;
            }
            let failed = run_round::<R, J>(&app);
            last_round = Some(Instant::now());
            changed_at = None;
            seen = Some(sky.st().event_count());
            retry_at = match failed {
                Some(CloudFail::Offline | CloudFail::Server | CloudFail::Space | CloudFail::Format) => Some(Instant::now() + RETRY),
                Some(CloudFail::Conflict | CloudFail::Newer) => Some(Instant::now() + Duration::from_secs(6 * 3600)),
                _ => None,
            };
        }
    });
}

/// Подключение для другого способа входа, которое умеет «Карман»: облако по коду — тоже облако.
impl Cloud for Box<dyn Conn> {
    fn list(&self, dir: Dir) -> kit_core::Result<Vec<Entry>> {
        (**self).list(dir)
    }
    fn get(&self, dir: Dir, name: &str, max: usize) -> kit_core::Result<Option<Vec<u8>>> {
        (**self).get(dir, name, max)
    }
    fn put(&self, dir: Dir, name: &str, data: &[u8]) -> kit_core::Result<()> {
        (**self).put(dir, name, data)
    }
    fn exists(&self, dir: Dir, name: &str) -> kit_core::Result<bool> {
        (**self).exists(dir, name)
    }
    fn delete(&self, dir: Dir, name: &str) -> kit_core::Result<()> {
        (**self).delete(dir, name)
    }
}

/// Команды экрана облака для базы приложения `$store`: `cloud_status`, `cloud_app_passwords`,
/// `cloud_dav_login`, `cloud_password`, `cloud_sync_now`, `cloud_every`, `cloud_disconnect`.
/// Объявляются в модуле приложения и вписываются в `tauri::generate_handler!`.
#[macro_export]
macro_rules! cloud_commands {
    ($store:ty) => {
        #[::tauri::command]
        pub fn cloud_status(app: ::tauri::AppHandle) -> $crate::Res<$crate::cloud::Status> {
            $crate::cloud::status::<_, $store>(&app, None)
        }

        /// Страница Яндекс ID, где создают пароль приложения.
        #[::tauri::command]
        pub fn cloud_app_passwords(app: ::tauri::AppHandle) -> $crate::Res<()> {
            $crate::cloud::app_passwords(&app)
        }

        /// Вход по логину и паролю: Яндекс Диск (пароль приложения) или свой сервер WebDAV.
        #[::tauri::command]
        pub async fn cloud_dav_login(app: ::tauri::AppHandle, kind: String, url: Option<String>, user: String, password: String) -> $crate::Res<$crate::cloud::Status> {
            $crate::cloud::dav_login::<_, $store>(app, kind, url, user, password).await
        }

        /// Пароль копий: открывает ключ облака, которое уже есть, или создаёт новое.
        #[::tauri::command]
        pub async fn cloud_password(app: ::tauri::AppHandle, password: String) -> $crate::Res<$crate::cloud::Status> {
            $crate::cloud::password::<_, $store>(app, password).await
        }

        #[::tauri::command]
        pub fn cloud_sync_now(app: ::tauri::AppHandle) -> $crate::Res<$crate::cloud::Status> {
            $crate::cloud::sync_button::<_, $store>(&app)
        }

        #[::tauri::command]
        pub fn cloud_every(app: ::tauri::AppHandle, every: String) -> $crate::Res<$crate::cloud::Status> {
            $crate::cloud::set_every::<_, $store>(&app, every)
        }

        /// Отключает облако на этом устройстве; `wipe` — ещё и удаляет данные из облака.
        #[::tauri::command]
        pub async fn cloud_disconnect(app: ::tauri::AppHandle, wipe: bool) -> $crate::Res<$crate::cloud::Status> {
            $crate::cloud::disconnect::<_, $store>(app, wipe).await
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_yandex_pages_open() {
        assert!(safe_page("https://id.yandex.ru/security"));
        assert!(!safe_page("http://id.yandex.ru/security"));
        assert!(!safe_page("https://id.yandex.ru.evil.example/security"));
        assert!(!safe_page("https://evil.example/?https://id.yandex.ru/"));
        assert!(!safe_page("https://id.yandex.ru/security\" onclick"));
        assert!(!safe_page("file:///etc/passwd"));
    }

    #[test]
    fn links_by_password() {
        for l in [Link::YaDisk, Link::WebDav] {
            assert_eq!(Link::parse(l.code()), Some(l));
        }
        assert_eq!(Link::parse("dropbox"), None);
        assert_eq!(Link::YaDisk.title(), "Яндекс Диск");
    }

    #[test]
    fn last_exchange_of_old_versions_is_read() {
        let polka = r#"{"at":1,"received":2,"sent":3,"files_received":4,"files_sent":5,"files_waiting":6,"books_received":7,"books_sent":8,"books_waiting":9,"problems":[]}"#;
        let l: Last = serde_json::from_str(polka).unwrap();
        assert_eq!((l.whole_received, l.whole_sent, l.whole_waiting), (7, 8, 9));
        let karman = r#"{"at":1,"received":2,"sent":3,"files_received":4,"files_sent":5,"files_waiting":6}"#;
        let l: Last = serde_json::from_str(karman).unwrap();
        assert_eq!((l.whole_received, l.problems.len()), (0, 0));
    }
}
