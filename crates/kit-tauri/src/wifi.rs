//! Связь по Wi-Fi в приложении: связанные устройства, код связи, поиск в сети и обмен в фоне.
//! Одинаково у всех приложений на модуле; своё у приложения — настройки связи (`kit_core::lan::Lan`),
//! база и строки ключей для хранилища системы.
//!
//! - Связанные устройства и ключи пары — только на этом устройстве: в зашифрованной базе, ключи ещё
//!   и под защитой системы (DPAPI, Android Keystore). В обмен, облако и резервные копии не попадают.
//! - Сервер обмена и ответ на поиск работают, только пока есть связанные устройства или показан код.
//! - Связанные устройства в одной сети находят друг друга и обмениваются сами: вскоре после изменений
//!   и раз в минуту, пока приложение открыто.
//!
//! Подключение: приложение кладёт в состояние [`Wifi`] (`app.manage(Wifi::new(…))`), запускает фон
//! [`start`], зовёт [`on_lock`] при блокировке и объявляет команды экрана макросом
//! [`wifi_commands!`](crate::wifi_commands) — их имена одинаковы у всех приложений (`lan_status`, …).

use crate::guard::Lock;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
pub use kit_core::lan::Device;
use kit_core::lan::{self, CodeState, Found, Invite, Lan, Peer};
use kit_core::sync::SyncReport;
use kit_secure::KeyNames;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};

pub use crate::Res;

/// На устройстве (не синхронизируется): связанные устройства.
const META_PEERS: &str = "lan.peers";
/// Связанных устройств — не больше стольких.
const MAX_PEERS: usize = 16;
/// Сколько ждать ответов на поиск.
const SCAN_WAIT: Duration = Duration::from_millis(1200);
/// Устройство «в сети», если его видели за это время.
const ONLINE: Duration = Duration::from_secs(90);
/// Обмен с найденным устройством — раз в минуту и через 5 секунд после своих изменений
/// (в отладочной сборке — `<ID>_LAN_EVERY_SECS` и `<ID>_LAN_CHANGE_SECS`).
const EVERY: u64 = 60;
const AFTER_CHANGE: u64 = 5;

/// Связанное устройство, как оно хранится.
#[derive(Clone, Serialize, Deserialize)]
struct PeerRec {
    id: String,
    name: String,
    /// Ключ пары под защитой системы, base64.
    key: String,
    /// Когда связали (unix).
    paired: i64,
    /// Последний удачный обмен (unix).
    #[serde(default)]
    last: Option<i64>,
    /// Где видели в последний раз: «192.168.1.34:47510».
    #[serde(default)]
    addr: Option<String>,
    #[serde(default)]
    app: String,
    /// Почему не получился последний обмен.
    #[serde(default)]
    error: Option<String>,
}

/// Связь по Wi-Fi в приложении: настройки, база и что сейчас происходит.
pub struct Wifi<J> {
    lan: &'static Lan,
    keys: &'static KeyNames,
    store: Arc<Mutex<J>>,
    id: String,
    name: String,
    code: Arc<Mutex<Option<CodeState>>>,
    /// Сервер и ответчик на поиск.
    net: Mutex<Option<(lan::Server, Option<lan::Responder>)>>,
    /// Найдено при последнем поиске.
    found: Mutex<Vec<Found>>,
    /// Когда видели связанное устройство (найдено или был обмен).
    seen: Mutex<BTreeMap<String, Instant>>,
    /// С кем сейчас идёт обмен.
    busy: Mutex<Vec<String>>,
    /// Что сказать под кодом: «Связано с …», «Код отменён» — и когда (через две минуты не показываем).
    note: Mutex<Option<(String, Instant)>>,
    manual: AtomicBool,
    wake: (Mutex<bool>, Condvar),
}

impl<J: Device> Wifi<J> {
    /// `id` и `name` — номер и имя этого устройства, `keys` — строки ключей приложения для хранилища системы.
    pub fn new(lan: &'static Lan, keys: &'static KeyNames, store: Arc<Mutex<J>>, id: String, name: String) -> Wifi<J> {
        Wifi {
            lan,
            keys,
            store,
            id,
            name,
            code: Arc::default(),
            net: Mutex::default(),
            found: Mutex::default(),
            seen: Mutex::default(),
            busy: Mutex::default(),
            note: Mutex::default(),
            manual: AtomicBool::new(false),
            wake: (Mutex::new(false), Condvar::new()),
        }
    }

    /// Поискать и обменяться при первой возможности (после своих изменений или по кнопке).
    pub fn wake(&self) {
        self.manual.store(true, Ordering::SeqCst);
        *lock(&self.wake.0) = true;
        self.wake.1.notify_all();
    }

    fn sleep_or_wake(&self, d: Duration) {
        let g = lock(&self.wake.0);
        if let Ok((mut g, _)) = self.wake.1.wait_timeout_while(g, d, |woken| !*woken) {
            *g = false;
        }
    }

    fn load(&self) -> Vec<PeerRec> {
        lock(&self.store).local(META_PEERS).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(st: &J, peers: &[PeerRec]) -> Res<()> {
        st.set_local(META_PEERS, &serde_json::to_string(peers).map_err(err)?).map_err(err)
    }

    fn key_of(&self, rec: &PeerRec) -> Option<[u8; 32]> {
        let blob = B64.decode(&rec.key).ok()?;
        kit_secure::unprotect(self.keys, &blob).ok()?.try_into().ok()
    }

    /// Обновить запись связанного устройства, если оно ещё связано.
    fn update(&self, id: &str, f: impl FnOnce(&mut PeerRec)) {
        let st = lock(&self.store);
        let mut peers: Vec<PeerRec> = st.local(META_PEERS).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        if let Some(p) = peers.iter_mut().find(|p| p.id == id) {
            f(p);
            let _ = Self::save(&st, &peers);
        }
    }

    /// Запомнить связанное устройство (или обновить ключ, если связали заново).
    fn remember(&self, peer: &Peer, key: &[u8; 32]) -> Res<()> {
        let blob = kit_secure::protect(self.keys, key).map_err(|e| format!("ключ связи не сохранить в хранилище системы: {e}"))?;
        let st = lock(&self.store);
        let mut peers: Vec<PeerRec> = st.local(META_PEERS).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        peers.retain(|p| p.id != peer.device);
        if peers.len() >= MAX_PEERS {
            return Err(format!("Связанных устройств — не больше {MAX_PEERS}: отвяжите лишние"));
        }
        peers.push(PeerRec { id: peer.device.clone(), name: peer.name.clone(), key: B64.encode(blob), paired: now(), last: None, addr: None, app: peer.app.clone(), error: None });
        Self::save(&st, &peers)?;
        drop(st);
        lock(&self.seen).insert(peer.device.clone(), Instant::now());
        Ok(())
    }

    fn tcp_port(&self) -> Option<u16> {
        lock(&self.net).as_ref().map(|(s, _)| s.port)
    }

    /// Порт поиска: в отладочной сборке — `<ID>_LAN_UDP` (второе устройство на этом же компьютере).
    fn discovery_port(&self) -> u16 {
        #[cfg(debug_assertions)]
        if let Some(p) = std::env::var(self.lan.names.env("LAN_UDP")).ok().and_then(|v| v.parse().ok()) {
            return p;
        }
        self.lan.discovery_port
    }

    /// Куда слать поиск: широковещательные адреса сетей устройства и общий 255.255.255.255.
    fn targets(&self) -> Vec<SocketAddr> {
        let port = self.lan.discovery_port;
        let mut out = vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), port)];
        for i in if_addrs::get_if_addrs().unwrap_or_default() {
            if i.is_loopback() {
                continue;
            }
            if let if_addrs::IfAddr::V4(v4) = &i.addr {
                let b = v4.broadcast.unwrap_or_else(|| Ipv4Addr::from(u32::from(v4.ip) | !u32::from(v4.netmask)));
                let t = SocketAddr::new(IpAddr::V4(b), port);
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
        // В отладочной сборке — ещё адреса для проверок (второе устройство на этом же компьютере).
        #[cfg(debug_assertions)]
        if let Ok(v) = std::env::var(self.lan.names.env("LAN_PROBE")) {
            out.extend(v.split(',').filter_map(|a| a.trim().parse::<SocketAddr>().ok()));
        }
        out
    }

    /// Адрес, введённый руками: только домашняя сеть.
    fn parse_addr(&self, a: &str) -> Res<SocketAddr> {
        let a = a.trim();
        let (host, port) = match a.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h, p.parse::<u16>().map_err(|_| format!("Порт — число, например 192.168.1.34:{}", self.lan.tcp_port))?),
            _ => (a, self.lan.tcp_port),
        };
        if !lan::is_local_address(host) {
            return Err("Адрес — из домашней сети, как на экране того устройства: например 192.168.1.34".into());
        }
        Ok(SocketAddr::new(host.parse::<IpAddr>().map_err(err)?, port))
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn wifi<R: Runtime, J: Device>(app: &AppHandle<R>) -> tauri::State<'_, Wifi<J>> {
    app.state::<Wifi<J>>()
}

/// Приложение не заблокировано (если вход по PIN-коду подключён).
fn unlocked<R: Runtime>(app: &AppHandle<R>) -> Res<()> {
    match app.try_state::<Lock>() {
        Some(l) => l.check(),
        None => Ok(()),
    }
}

/// Сколько записей пришло за обмен: события, вложения и большие файлы.
fn received(r: &SyncReport) -> usize {
    r.received + r.files_received + r.whole_received
}

// ——— Адреса, QR-код и версии ———

/// Адреса этого устройства в сети (IPv4): сначала домашние сети и точка доступа телефона.
pub fn local_ipv4() -> Vec<String> {
    let mut out: Vec<String> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            IpAddr::V4(v4) if !v4.is_link_local() => Some(v4.to_string()),
            _ => None,
        })
        .collect();
    out.sort_by_key(|ip| if ip.starts_with("192.168.") { 0 } else if ip.starts_with("10.") { 1 } else { 2 });
    out.dedup();
    out
}

/// QR-код картинкой SVG.
pub fn qr_svg(text: &str) -> Res<String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(err)?;
    Ok(code.render::<svg::Color>().min_dimensions(232, 232).dark_color(svg::Color("#15232D")).light_color(svg::Color("#FFFFFF")).build())
}

fn version_num(v: &str) -> (u32, u32, u32) {
    let mut it = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// Что сказать, если версии на устройствах разные.
pub fn version_hint(mine: &str, peer_app: &str) -> String {
    if peer_app.is_empty() {
        return String::new();
    }
    match version_num(peer_app).cmp(&version_num(mine)) {
        std::cmp::Ordering::Less => format!("На другом устройстве версия {peer_app} — обновите его до {mine}."),
        std::cmp::Ordering::Greater => format!("На другом устройстве версия {peer_app}, новее этой ({mine}) — обновите это устройство."),
        std::cmp::Ordering::Equal => String::new(),
    }
}

/// Ошибка модуля — предложением: с большой буквы.
pub fn sentence(e: &kit_core::Error) -> String {
    let m = e.to_string();
    let mut c = m.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => m,
    }
}

fn pair_error(e: kit_core::Error) -> String {
    match e {
        kit_core::Error::Lan(_, m) | kit_core::Error::Peer(m) => m,
        kit_core::Error::Io(e) => format!("Не удалось подключиться: {e}. Устройства должны быть в одной сети Wi-Fi"),
        e => format!("Не удалось связаться: {e}"),
    }
}

// ——— Фон ———

#[derive(Clone, Serialize)]
struct LanEvent {
    /// Сколько записей пришло — обновить сводку.
    received: usize,
    /// Связались по коду с этим устройством.
    paired: Option<String>,
}

/// Обмен закончился — запомнить, когда и с какой версией, и сказать экрану.
fn done<R: Runtime, J: Device>(app: &AppHandle<R>, id: &str, r: &Res<SyncReport>) {
    let w = wifi::<R, J>(app);
    lock(&w.seen).insert(id.to_string(), Instant::now());
    match r {
        Ok(rep) => w.update(id, |p| {
            p.last = Some(now());
            p.error = None;
            if !rep.peer_app.is_empty() {
                p.app = rep.peer_app.clone();
            }
            if !rep.peer_name.is_empty() {
                p.name = rep.peer_name.chars().filter(|c| !c.is_control()).take(64).collect();
            }
        }),
        Err(e) => w.update(id, |p| p.error = Some(e.clone())),
    }
    let _ = app.emit("lan", LanEvent { received: r.as_ref().map(received).unwrap_or(0), paired: None });
}

/// Устройство в сети для сервера и ответчика: ключи пары и запоминание — из базы этого устройства.
fn host<R: Runtime, J: Device>(app: &AppHandle<R>) -> lan::Host<J> {
    let w = wifi::<R, J>(app);
    let (a1, a2, a3) = (app.clone(), app.clone(), app.clone());
    lan::Host {
        lan: w.lan,
        store: w.store.clone(),
        device: w.id.clone(),
        name: w.name.clone(),
        code: w.code.clone(),
        peer_key: Arc::new(move |id| {
            let w = wifi::<R, J>(&a1);
            let rec = w.load().into_iter().find(|p| p.id == id)?;
            w.key_of(&rec)
        }),
        paired: Arc::new(move |peer, key| {
            let w = wifi::<R, J>(&a2);
            w.remember(peer, key)?;
            *lock(&w.note) = Some((format!("Связано с «{}»", peer.name), Instant::now()));
            let _ = a2.emit("lan", LanEvent { received: 0, paired: Some(peer.name.clone()) });
            Ok(())
        }),
        synced: Arc::new(move |id, r| done::<R, J>(&a3, id, &r.map_err(|e| e.to_string()))),
    }
}

/// Сервер и ответчик — пока есть связанные устройства или показан код; иначе порты закрыты.
fn ensure_net<R: Runtime, J: Device>(app: &AppHandle<R>) {
    let w = wifi::<R, J>(app);
    let want = lock(&w.code).as_ref().is_some_and(CodeState::active) || !w.load().is_empty();
    let mut net = lock(&w.net);
    match (want, net.is_some()) {
        (true, false) => {
            let h = host::<R, J>(app);
            match lan::Server::start(h.clone(), w.lan.tcp_port) {
                Ok(server) => {
                    // Порт поиска занят (второе такое же приложение на компьютере) — нас просто не найдут,
                    // связь по адресу работает.
                    let responder = lan::Responder::start(h, w.discovery_port(), server.port).ok();
                    *net = Some((server, responder));
                }
                Err(e) => eprintln!("обмен по Wi-Fi: порт не открыть: {e}"),
            }
        }
        (false, true) => {
            if let Some((server, responder)) = net.take() {
                server.stop();
                if let Some(r) = responder {
                    r.stop();
                }
            }
        }
        _ => {}
    }
}

/// Поиск в сети: устройства с показанным кодом и связанные.
fn scan<R: Runtime, J: Device>(app: &AppHandle<R>) -> Vec<Found> {
    let w = wifi::<R, J>(app);
    let keys: Vec<(String, [u8; 32])> = w.load().iter().filter_map(|p| w.key_of(p).map(|k| (p.id.clone(), k))).collect();
    let found = lan::discover(w.lan, &w.targets(), &w.id, &keys, SCAN_WAIT).unwrap_or_default();
    {
        let mut seen = lock(&w.seen);
        for f in found.iter().filter(|f| f.peer) {
            seen.insert(f.device.clone(), Instant::now());
        }
    }
    for f in found.iter().filter(|f| f.peer) {
        let addr = f.addr.to_string();
        w.update(&f.device, |p| p.addr = Some(addr));
    }
    *lock(&w.found) = found.clone();
    found
}

/// Обмен с одним связанным устройством.
fn sync_one<R: Runtime, J: Device>(app: &AppHandle<R>, rec: &PeerRec, addr: SocketAddr) -> Res<SyncReport> {
    let w = wifi::<R, J>(app);
    {
        let mut busy = lock(&w.busy);
        if busy.contains(&rec.id) {
            return Err(format!("С «{}» уже идёт обмен", rec.name));
        }
        busy.push(rec.id.clone());
    }
    let key = w.key_of(rec).ok_or_else(|| "ключ связи не достать из хранилища системы: свяжите устройства заново".to_string());
    let r = key.and_then(|key| {
        lan::sync_peer(w.lan, addr, &w.id, &rec.id, &rec.name, &key, &*w.store, &w.name).map_err(|e| match e {
            kit_core::Error::Lan(_, m) | kit_core::Error::Peer(m) => m,
            e => format!("Не удалось обменяться с «{}»: {e}", rec.name),
        })
    });
    lock(&w.busy).retain(|id| id != &rec.id);
    done::<R, J>(app, &rec.id, &r);
    r
}

fn secs<J>(w: &Wifi<J>, what: &str, default: u64) -> Duration {
    #[cfg(debug_assertions)]
    if let Some(s) = std::env::var(w.lan.names.env(what)).ok().and_then(|v| v.parse().ok()) {
        return Duration::from_secs(s);
    }
    let _ = (w, what);
    Duration::from_secs(default)
}

/// Фон: открыть порты, когда нужно, находить связанные устройства и обмениваться.
pub fn start<R: Runtime, J: Device>(app: AppHandle<R>) {
    std::thread::spawn(move || {
        let w = wifi::<R, J>(&app);
        let tick = secs(&w, "LAN_TICK_SECS", 15);
        let every = secs(&w, "LAN_EVERY_SECS", EVERY);
        let after_change = secs(&w, "LAN_CHANGE_SECS", AFTER_CHANGE);
        let mut last_events: Option<u64> = None;
        let mut changed_at: Option<Instant> = None;
        let mut last_round: Option<Instant> = None;
        w.sleep_or_wake(secs(&w, "LAN_DELAY_SECS", 3));
        loop {
            ensure_net::<R, J>(&app);
            let peers = w.load();
            let events = lock(&w.store).event_count();
            let manual = w.manual.swap(false, Ordering::SeqCst);
            if last_events.is_some_and(|e| e != events) && changed_at.is_none() {
                changed_at = Some(Instant::now());
            }
            last_events = Some(events);
            let due = manual || last_round.is_none_or(|t| t.elapsed() >= every) || changed_at.is_some_and(|t| t.elapsed() >= after_change);
            if !peers.is_empty() && due {
                let found = scan::<R, J>(&app);
                for rec in &peers {
                    if let Some(f) = found.iter().find(|f| f.peer && f.device == rec.id) {
                        let _ = sync_one::<R, J>(&app, rec, f.addr);
                    }
                }
                last_round = Some(Instant::now());
                changed_at = None;
                last_events = Some(lock(&w.store).event_count());
            }
            w.sleep_or_wake(tick);
        }
    });
}

/// Приложение заблокировалось: код связи больше не действует — по прежнему коду или QR-коду
/// не подключиться. Обмен со связанными устройствами продолжается, как с облаком.
pub fn on_lock<R: Runtime, J: Device>(app: &AppHandle<R>) {
    if let Some(w) = app.try_state::<Wifi<J>>() {
        lock(&w.code).take();
    }
}

/// Поискать и обменяться при первой возможности: после своих изменений или по кнопке.
pub fn wake<R: Runtime, J: Device>(app: &AppHandle<R>) {
    if let Some(w) = app.try_state::<Wifi<J>>() {
        w.wake();
    }
}

// ——— Экран ———

#[derive(Serialize)]
pub struct CodeView {
    /// «6WVG-HEWK»
    code: String,
    /// Сколько секунд ещё действует.
    secs: u64,
    /// Сколько попыток осталось.
    left: u8,
    wrong: u8,
    qr_svg: String,
    hosts: Vec<String>,
    port: u16,
}

#[derive(Serialize)]
pub struct PeerView {
    id: String,
    name: String,
    online: bool,
    syncing: bool,
    last: Option<i64>,
    app: String,
    error: Option<String>,
}

#[derive(Serialize)]
pub struct NearbyView {
    device: String,
    name: String,
    addr: String,
}

#[derive(Serialize)]
pub struct LanView {
    /// Имя и адреса этого устройства.
    name: String,
    hosts: Vec<String>,
    code: Option<CodeView>,
    /// Под кодом: «Связано с …», «Код отменён…».
    note: Option<String>,
    peers: Vec<PeerView>,
    /// Устройства с показанным кодом, ещё не связанные.
    nearby: Vec<NearbyView>,
}

#[derive(Serialize)]
pub struct PairResult {
    pub peer_name: String,
    pub sent: usize,
    pub received: usize,
    pub files_sent: usize,
    pub files_received: usize,
    /// Подсказка: другая версия на том устройстве, QR прежней версии.
    pub hint: String,
    /// Устройство запомнено (а не разовый обмен по QR прежней версии).
    pub remembered: bool,
}

impl PairResult {
    fn of(lan: &Lan, peer_name: String, r: &SyncReport) -> PairResult {
        PairResult {
            peer_name,
            sent: r.sent,
            received: r.received,
            files_sent: r.files_sent + r.whole_sent,
            files_received: r.files_received + r.whole_received,
            hint: version_hint(lan.profile.version, &r.peer_app),
            remembered: true,
        }
    }
}

fn view<R: Runtime, J: Device>(app: &AppHandle<R>) -> Res<LanView> {
    let w = wifi::<R, J>(app);
    let hosts = local_ipv4();
    let port = w.tcp_port().unwrap_or(w.lan.tcp_port);
    let code = {
        let mut c = lock(&w.code);
        match c.as_ref() {
            Some(st) if st.active() => {
                let invite = Invite { device: w.id.clone(), name: w.name.clone(), hosts: hosts.clone(), port, code: st.code.clone() };
                Some(CodeView { code: lan::show_code(&st.code), secs: st.until.saturating_duration_since(Instant::now()).as_secs(), left: st.left, wrong: st.wrong, qr_svg: qr_svg(&invite.to_uri(w.lan))?, hosts: hosts.clone(), port })
            }
            Some(st) => {
                // Код истёк или отменён: убрать и сказать почему.
                let why = if st.left == 0 { "Код отменён: было пять неверных попыток. Покажите новый" } else { "Код больше не действует. Чтобы связать устройства, покажите новый" };
                *lock(&w.note) = Some((why.into(), Instant::now()));
                c.take();
                None
            }
            None => None,
        }
    };
    let peers = w.load();
    let seen = lock(&w.seen).clone();
    let busy = lock(&w.busy).clone();
    let found = lock(&w.found).clone();
    let note = lock(&w.note).as_ref().filter(|(_, t)| t.elapsed() < Duration::from_secs(120)).map(|(n, _)| n.clone());
    Ok(LanView {
        name: w.name.clone(),
        hosts,
        code,
        note,
        peers: peers
            .iter()
            .map(|p| PeerView { id: p.id.clone(), name: p.name.clone(), online: seen.get(&p.id).is_some_and(|t| t.elapsed() < ONLINE), syncing: busy.contains(&p.id), last: p.last, app: p.app.clone(), error: p.error.clone() })
            .collect(),
        nearby: found.iter().filter(|f| f.pairing && !peers.iter().any(|p| p.id == f.device)).map(|f| NearbyView { device: f.device.clone(), name: f.name.clone(), addr: f.addr.to_string() }).collect(),
    })
}

/// Связь по Wi-Fi: код, связанные устройства, кто рядом.
pub fn status<R: Runtime, J: Device>(app: &AppHandle<R>) -> Res<LanView> {
    unlocked(app)?;
    view::<R, J>(app)
}

/// Поискать в сети сейчас (около секунды) — устройства с кодом и связанные.
pub async fn scan_now<R: Runtime, J: Device>(app: AppHandle<R>) -> Res<LanView> {
    unlocked(&app)?;
    let a = app.clone();
    tauri::async_runtime::spawn_blocking(move || scan::<R, J>(&a)).await.map_err(err)?;
    view::<R, J>(&app)
}

/// Показать код связи: восемь знаков на пять минут, один раз.
pub fn code_start<R: Runtime, J: Device>(app: &AppHandle<R>) -> Res<LanView> {
    unlocked(app)?;
    let w = wifi::<R, J>(app);
    if local_ipv4().is_empty() {
        return Err("Нет подключения к сети: включите Wi-Fi или точку доступа".into());
    }
    *lock(&w.code) = Some(CodeState::new(lan::new_code().map_err(|e| sentence(&e))?));
    *lock(&w.note) = None;
    ensure_net::<R, J>(app);
    if w.tcp_port().is_none() {
        lock(&w.code).take();
        let n = w.lan.names;
        return Err(format!("Не открыть порт для обмена: возможно, его {} {} «{}» на этом компьютере", n.g("занял", "заняла"), n.g("другой", "другая"), n.name));
    }
    view::<R, J>(app)
}

pub fn code_stop<R: Runtime, J: Device>(app: &AppHandle<R>) -> Res<LanView> {
    unlocked(app)?;
    let w = wifi::<R, J>(app);
    lock(&w.code).take();
    *lock(&w.note) = None;
    ensure_net::<R, J>(app);
    view::<R, J>(app)
}

/// Связь по коду: с найденным устройством (`device`) или по адресу (`addr`: «192.168.1.34» или с портом).
pub async fn pair<R: Runtime, J: Device>(app: AppHandle<R>, device: Option<String>, addr: Option<String>, code: String) -> Res<PairResult> {
    unlocked(&app)?;
    let code = lan::normalize_code(&code).map_err(|e| sentence(&e))?;
    let target: SocketAddr = {
        let w = wifi::<R, J>(&app);
        match (device, addr) {
            (Some(d), _) => lock(&w.found).iter().find(|f| f.device == d && f.pairing).map(|f| f.addr).ok_or("Устройство пропало из сети: найдите его снова")?,
            (None, Some(a)) => w.parse_addr(&a)?,
            _ => return Err("Выберите устройство или введите его адрес".into()),
        }
    };
    pair_at::<R, J>(app, vec![target], code).await
}

/// Связь по коду с первым отозвавшимся адресом: ключ пары, запоминание и первый обмен.
pub async fn pair_at<R: Runtime, J: Device>(app: AppHandle<R>, addrs: Vec<SocketAddr>, code: String) -> Res<PairResult> {
    let a = app.clone();
    let (peer, rep) = tauri::async_runtime::spawn_blocking(move || {
        let w = wifi::<R, J>(&a);
        let mut last = None;
        for addr in addrs {
            match lan::pair_join(w.lan, addr, &code, &*w.store, &w.id, &w.name, &|peer, key| w.remember(peer, key)) {
                Ok((peer, rep)) => {
                    done::<R, J>(&a, &peer.device, &Ok(rep.clone()));
                    let _ = a.emit("lan", LanEvent { received: received(&rep), paired: Some(peer.name.clone()) });
                    return Ok((peer, rep));
                }
                // Нет связи по этому адресу — пробуем следующий; ответ устройства — окончательный.
                Err(kit_core::Error::Io(e)) => last = Some(kit_core::Error::Io(e)),
                Err(e) => return Err(pair_error(e)),
            }
        }
        Err(last.map(pair_error).unwrap_or_else(|| "Нет адреса для подключения".into()))
    })
    .await
    .map_err(err)??;
    let w = wifi::<R, J>(&app);
    w.wake();
    Ok(PairResult::of(w.lan, peer.name, &rep))
}

/// QR-код с другого устройства: адрес и код связи.
pub async fn pair_qr<R: Runtime, J: Device>(app: AppHandle<R>, uri: String) -> Res<PairResult> {
    unlocked(&app)?;
    let (addrs, code) = {
        let w = wifi::<R, J>(&app);
        let inv = Invite::parse(w.lan, &uri).map_err(|e| sentence(&e))?;
        if inv.device == w.id {
            return Err("Это QR-код этого же устройства. Отсканируйте его другим устройством".into());
        }
        let addrs: Vec<SocketAddr> = inv.hosts.iter().filter_map(|h| h.parse::<IpAddr>().ok()).map(|ip| SocketAddr::new(ip, inv.port)).collect();
        (addrs, inv.code)
    };
    pair_at::<R, J>(app, addrs, code).await
}

/// Обменяться со связанным устройством сейчас.
pub async fn sync_now<R: Runtime, J: Device>(app: AppHandle<R>, id: String) -> Res<PairResult> {
    unlocked(&app)?;
    let a = app.clone();
    let (name, r) = tauri::async_runtime::spawn_blocking(move || {
        let w = wifi::<R, J>(&a);
        let rec = w.load().into_iter().find(|p| p.id == id).ok_or("Устройство уже отвязано")?;
        // Где устройство сейчас: поиск в сети, а не нашли — прежний адрес.
        let found = scan::<R, J>(&a);
        let addr = found.iter().find(|f| f.peer && f.device == rec.id).map(|f| f.addr).or_else(|| rec.addr.as_deref().and_then(|x| x.parse().ok()));
        let addr = addr.ok_or_else(|| format!("«{}» не видно в сети: откройте «{}» там и проверьте, что устройства в одной сети Wi-Fi", rec.name, w.lan.names.name_acc))?;
        sync_one::<R, J>(&a, &rec, addr).map(|r| (rec.name.clone(), r))
    })
    .await
    .map_err(err)??;
    Ok(PairResult::of(wifi::<R, J>(&app).lan, name, &r))
}

/// Отвязать устройство: обмен по Wi-Fi с ним прекращается, записи остаются на обоих.
pub fn unpair<R: Runtime, J: Device>(app: &AppHandle<R>, id: String) -> Res<LanView> {
    unlocked(app)?;
    let w = wifi::<R, J>(app);
    {
        let st = lock(&w.store);
        let mut peers: Vec<PeerRec> = st.local(META_PEERS).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        peers.retain(|p| p.id != id);
        Wifi::<J>::save(&st, &peers)?;
    }
    lock(&w.seen).remove(&id);
    ensure_net::<R, J>(app);
    view::<R, J>(app)
}

/// Команды экрана связи по Wi-Fi для базы приложения `$store`: `lan_status`, `lan_scan`, `lan_code_start`,
/// `lan_code_stop`, `lan_pair`, `lan_pair_qr`, `lan_sync_now`, `lan_unpair`. Объявляются в модуле приложения
/// и вписываются в `tauri::generate_handler!`. Приложение со своим разбором QR-кода (как «Карман»
/// с QR прежней версии) пишет `lan_pair_qr` само — тогда его нужно указать после `$store; no_qr`.
#[macro_export]
macro_rules! wifi_commands {
    ($store:ty) => {
        $crate::wifi_commands!($store; no_qr);

        /// QR-код с другого устройства: адрес и код связи.
        #[::tauri::command]
        pub async fn lan_pair_qr(app: ::tauri::AppHandle, uri: String) -> $crate::wifi::Res<$crate::wifi::PairResult> {
            $crate::wifi::pair_qr::<_, $store>(app, uri).await
        }
    };
    ($store:ty; no_qr) => {
        /// Связь по Wi-Fi: код, связанные устройства, кто рядом.
        #[::tauri::command]
        pub fn lan_status(app: ::tauri::AppHandle) -> $crate::wifi::Res<$crate::wifi::LanView> {
            $crate::wifi::status::<_, $store>(&app)
        }

        /// Поискать в сети сейчас (около секунды).
        #[::tauri::command]
        pub async fn lan_scan(app: ::tauri::AppHandle) -> $crate::wifi::Res<$crate::wifi::LanView> {
            $crate::wifi::scan_now::<_, $store>(app).await
        }

        /// Показать код связи: восемь знаков на пять минут, один раз.
        #[::tauri::command]
        pub fn lan_code_start(app: ::tauri::AppHandle) -> $crate::wifi::Res<$crate::wifi::LanView> {
            $crate::wifi::code_start::<_, $store>(&app)
        }

        #[::tauri::command]
        pub fn lan_code_stop(app: ::tauri::AppHandle) -> $crate::wifi::Res<$crate::wifi::LanView> {
            $crate::wifi::code_stop::<_, $store>(&app)
        }

        /// Связь по коду: с найденным устройством или по адресу.
        #[::tauri::command]
        pub async fn lan_pair(app: ::tauri::AppHandle, device: Option<String>, addr: Option<String>, code: String) -> $crate::wifi::Res<$crate::wifi::PairResult> {
            $crate::wifi::pair::<_, $store>(app, device, addr, code).await
        }

        /// Обменяться со связанным устройством сейчас.
        #[::tauri::command]
        pub async fn lan_sync_now(app: ::tauri::AppHandle, id: String) -> $crate::wifi::Res<$crate::wifi::PairResult> {
            $crate::wifi::sync_now::<_, $store>(app, id).await
        }

        /// Отвязать устройство: обмен по Wi-Fi с ним прекращается, записи остаются на обоих.
        #[::tauri::command]
        pub fn lan_unpair(app: ::tauri::AppHandle, id: String) -> $crate::wifi::Res<$crate::wifi::LanView> {
            $crate::wifi::unpair::<_, $store>(&app, id)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_qr() {
        assert_eq!(version_hint("0.15.9", "0.15.9"), "");
        assert_eq!(version_hint("0.15.9", ""), "");
        assert!(version_hint("0.15.9", "0.0.1").contains("обновите его до 0.15.9"));
        assert!(version_hint("0.15.9", "99.0.0").contains("обновите это устройство"));
        assert!(qr_svg("polka://pair?v=1&d=a").unwrap().starts_with("<?xml"));
    }

    #[test]
    fn errors_read_as_sentences() {
        let e = lan::normalize_code("234").unwrap_err();
        assert_eq!(sentence(&e), "В коде восемь знаков, например 6WVG-HEWK");
        assert_eq!(pair_error(kit_core::Error::Peer("Код не подошёл".into())), "Код не подошёл");
    }
}
