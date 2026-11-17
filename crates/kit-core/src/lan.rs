//! Связь по Wi-Fi: поиск устройств в сети, связь по коду и обмен со связанными устройствами.
//!
//! Устроено как в «Кармане» 0.15 и «Полке» 0.11. У каждого приложения свои порты, метки и строки
//! ключей ([`Lan`]) — чтобы приложения в одной сети не мешали друг другу и ключи одного не подходили
//! к другому. Сам обмен записями — [`crate::sync`].
//!
//! **Связь по коду.** Одно устройство показывает код из восьми знаков ([`new_code`]), другое его
//! вводит. Из кода оба получают общий ключ по SPAKE2: сам код по сети не идёт, а тот, кто подслушал
//! обмен, не может проверить у себя ни одного варианта кода. Каждая попытка — только подключением
//! к устройству с кодом; после пяти неверных код отменяется. Ключ подтверждают обе стороны, и только
//! потом идёт что-то ещё: подставное устройство, не знающее кода, ничего не получит.
//!
//! **Ключ пары.** Из ключа SPAKE2 оба выводят ключ пары и запоминают его вместе с номером другого
//! устройства. Дальше обмен — по этому ключу: у каждого подключения свои случайные числа обеих сторон,
//! ключи сеанса — свои для каждого направления, у каждого пакета — свой номер. Повтор старого
//! подключения, перестановка и подмена пакетов не проходят.
//!
//! **Поиск в сети.** Запрос — широковещательный пакет UDP. Отвечают только устройство с показанным
//! кодом (имя и порт — чтобы выбрать его в списке) и связанные устройства: им запрос подписан ключом
//! пары, и ответ тоже. Чужим в сети — в гостинице, в кафе — приложение не отвечает.

use crate::error::{Error, Result};
use crate::sync::{prepare, recv_max, run_client, run_server, send, Journal, Msg, Profile, SyncReport, Wire, MAX_FRAME};
use crate::AppNames;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use blake2::digest::consts::U32;
use blake2::digest::Mac;
use blake2::Blake2bMac;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::Serialize;
use serde_json::{json, Value};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Код действует пять минут.
pub const CODE_TTL: Duration = Duration::from_secs(5 * 60);
/// Неверных попыток до отмены кода.
pub const CODE_TRIES: u8 = 5;
/// Знаки кода: латиница и цифры без похожих друг на друга 0 и O, 1 и I — 32 знака, 5 бит на знак.
pub const CODE_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
/// Знаков в коде: 40 бит — с пятью попытками подобрать нельзя.
pub const CODE_LEN: usize = 8;

/// Версия открытых сообщений и пакетов поиска.
const VERSION: u8 = 1;
/// Открытые сообщения (до ключа) маленькие и ждём их недолго.
const PLAIN_MAX: usize = 4096;
const PLAIN_WAIT: Duration = Duration::from_secs(10);
/// Подключение к устройству в сети.
const CONNECT_WAIT: Duration = Duration::from_secs(4);
/// Одновременных подключений к серверу не больше стольких — остальные закрываются сразу.
const MAX_CONNECTIONS: usize = 4;
/// Запросов поиска в секунду разбираем не больше стольких — остальные отбрасываем не глядя.
const MAX_QUERIES: usize = 60;

/// Связь по Wi-Fi одного приложения: порты, метки и строки ключей. Менять их нельзя — иначе
/// устройства с прежней версией перестанут находить и узнавать друг друга.
#[derive(Clone, Copy, Debug)]
pub struct Lan {
    /// Название — для сообщений: «На другом устройстве более новая версия „Полки“».
    pub names: AppNames,
    /// Протокол обмена записями.
    pub profile: &'static Profile,
    /// Порт обмена (TCP).
    pub tcp_port: u16,
    /// Порт поиска в сети (UDP).
    pub discovery_port: u16,
    /// Метка открытых сообщений и пакетов сеанса (`PLKL`).
    pub magic: [u8; 4],
    /// Метка запроса поиска (`PLKQ`).
    pub query_magic: [u8; 4],
    /// Метка ответа на поиск (`PLKA`).
    pub answer_magic: [u8; 4],
    /// Имена сторон SPAKE2: того, кто вводит код, и того, кто его показывает.
    pub joiner: &'static [u8],
    pub host: &'static [u8],
    /// Личная строка BLAKE2b для вывода ключей (`polka-lan`), до 16 байт.
    pub personal: &'static [u8],
    /// Ответ на чужое подключение: «это не „Полка“».
    pub not_ours: &'static str,
    /// QR-код связи: начало адреса (`polka://pair?`) и версия, в которой в нём код связи
    /// (у «Полки» — 1, у «Кармана» — 2: до 0.15 в версии 1 был ключ).
    pub qr_scheme: &'static str,
    pub qr_version: u32,
}

impl Lan {
    /// MAC и вывод ключей: BLAKE2b с ключом и личной строкой приложения, части — с длиной,
    /// чтобы границы не сдвигались.
    pub fn mac(&self, key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
        let mut m = Blake2bMac::<U32>::new_with_salt_and_personal(key, &[], self.personal).expect("ключ MAC — 32 байта, личная строка — до 16");
        for p in parts {
            Mac::update(&mut m, &(p.len() as u32).to_be_bytes());
            Mac::update(&mut m, p);
        }
        m.finalize().into_bytes().into()
    }

    fn write_plain(&self, s: &mut TcpStream, kind: u8, body: &Value) -> Result<()> {
        let b = serde_json::to_vec(body)?;
        if b.len() > PLAIN_MAX {
            return Err(Error::Format("слишком большой запрос".into()));
        }
        let mut out = Vec::with_capacity(8 + b.len());
        out.extend_from_slice(&self.magic);
        out.push(kind);
        out.push(VERSION);
        out.extend_from_slice(&(b.len() as u16).to_be_bytes());
        out.extend_from_slice(&b);
        s.write_all(&out)?;
        s.flush()?;
        Ok(())
    }

    fn read_plain(&self, s: &mut TcpStream) -> Result<(u8, Value)> {
        let mut head = [0u8; 8];
        s.read_exact(&mut head)?;
        if head[..4] != self.magic {
            return Err(Error::Format(self.not_ours.into()));
        }
        if head[5] != VERSION {
            return Err(Error::Peer(format!("На другом устройстве более новая версия «{}»: обновите это устройство", self.names.name_gen)));
        }
        let len = u16::from_be_bytes([head[6], head[7]]) as usize;
        if len > PLAIN_MAX {
            return Err(Error::Format("слишком большой запрос".into()));
        }
        let mut b = vec![0u8; len];
        s.read_exact(&mut b)?;
        Ok((head[4], serde_json::from_slice(&b)?))
    }

    fn datagram(&self, magic: &[u8; 4], body: &Value) -> Vec<u8> {
        let mut out = magic.to_vec();
        out.push(VERSION);
        out.extend_from_slice(&serde_json::to_vec(body).unwrap_or_default());
        out
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Сравнение без утечки по времени.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| Error::Random(e.to_string()))?;
    Ok(b)
}

fn b64(v: &[u8]) -> String {
    B64.encode(v)
}

/// Поле base64 ровно нужной длины.
fn field<const N: usize>(v: &Value, name: &str) -> Result<[u8; N]> {
    let raw = v[name].as_str().and_then(|t| B64.decode(t).ok()).ok_or_else(|| Error::Format(format!("нет поля {name}")))?;
    raw.try_into().map_err(|_| Error::Format(format!("поле {name} неверной длины")))
}

/// Номер устройства: латиница, цифры, «-» и «_», до 64 знаков.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Имя устройства с другой стороны: без управляющих знаков, до 64 знаков.
fn clean_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(64).collect::<String>().trim().to_string()
}

// ——— Код связи ———

/// Новый код связи: восемь случайных знаков.
pub fn new_code() -> Result<String> {
    let mut b = [0u8; CODE_LEN];
    getrandom::fill(&mut b).map_err(|e| Error::Random(e.to_string()))?;
    Ok(b.iter().map(|x| CODE_ALPHABET[(x & 31) as usize] as char).collect())
}

/// Код для экрана: «6WVG-HEWK».
pub fn show_code(code: &str) -> String {
    if code.len() == CODE_LEN {
        format!("{}-{}", &code[..4], &code[4..])
    } else {
        code.to_string()
    }
}

/// Код, как его ввёл человек, — к виду «6WVGHEWK»: без пробелов и дефисов, заглавными, русские буквы,
/// похожие на латинские (с телефонной клавиатуры), — латинскими.
pub fn normalize_code(input: &str) -> Result<String> {
    let mut out = String::new();
    for c in input.chars() {
        if c.is_whitespace() || c == '-' || c == '–' || c == '—' {
            continue;
        }
        let up = c.to_uppercase().next().unwrap_or(c);
        let latin = match up {
            'А' => 'A',
            'В' => 'B',
            'Е' | 'Ё' => 'E',
            'К' => 'K',
            'М' => 'M',
            'Н' => 'H',
            'Р' => 'P',
            'С' => 'C',
            'Т' => 'T',
            'У' => 'Y',
            'Х' => 'X',
            'З' => '3',
            other => other,
        };
        if !latin.is_ascii() || !CODE_ALPHABET.contains(&(latin as u8)) {
            return Err(Error::Format(format!("в коде нет знака «{c}»: только латинские буквы и цифры с экрана того устройства")));
        }
        out.push(latin);
    }
    if out.len() != CODE_LEN {
        return Err(Error::Format("в коде восемь знаков, например 6WVG-HEWK".into()));
    }
    Ok(out)
}

/// Адрес домашней сети: частные диапазоны IPv4 (и точки доступа телефона), 100.64/10,
/// link-local и свой компьютер; для IPv6 — link-local и ULA. Имена хостов не подходят.
/// Так QR-код чужого с адресом в интернете не уведёт записи на чужой сервер.
pub fn is_local_address(host: &str) -> bool {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            let [a, b, ..] = v4.octets();
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || (a == 100 && (64..128).contains(&b))
        }
        Ok(IpAddr::V6(v6)) => {
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xffc0) == 0xfe80 || (first & 0xfe00) == 0xfc00
        }
        Err(_) => false,
    }
}

/// QR-код связи: адрес устройства в сети и код связи. Ключа в нём нет.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, serde::Deserialize)]
pub struct Invite {
    pub device: String,
    pub name: String,
    pub hosts: Vec<String>,
    pub port: u16,
    pub code: String,
}

impl Invite {
    /// Текст QR-кода: `polka://pair?v=1&d=…&n=…&h=…&p=…&c=…`.
    pub fn to_uri(&self, lan: &Lan) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!("{}v={}&d={}&n={}&h={}&p={}&c={}", lan.qr_scheme, lan.qr_version, self.device, URL_SAFE_NO_PAD.encode(self.name.as_bytes()), self.hosts.join(","), self.port, self.code)
    }

    /// Разбирает QR-код связи этого приложения. Другой версии — просьба обновиться
    /// (прежние версии, если они есть, приложение разбирает само до этого).
    pub fn parse(lan: &Lan, uri: &str) -> Result<Invite> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let bad = |m: &str| Error::Format(m.to_string());
        let Some(q) = uri.trim().strip_prefix(lan.qr_scheme) else {
            return Err(bad(&format!("это не QR-код «{}» для связи", lan.names.name_gen)));
        };
        let mut fields = BTreeMap::new();
        for part in q.split('&') {
            let (k, v) = part.split_once('=').ok_or_else(|| bad("испорченный QR-код связи"))?;
            fields.insert(k, v);
        }
        if fields.get("v").copied() != Some(lan.qr_version.to_string().as_str()) {
            return Err(bad(&format!("QR-код от более новой версии приложения: обновите «{}» на этом устройстве", lan.names.name_acc)));
        }
        let unreadable = || bad("имя устройства не читается");
        let name = match fields.get("n") {
            Some(v) => String::from_utf8(URL_SAFE_NO_PAD.decode(v).map_err(|_| unreadable())?).map_err(|_| unreadable())?,
            None => String::new(),
        };
        let hosts: Vec<String> = fields.get("h").map(|h| h.split(',').filter(|h| !h.is_empty()).map(String::from).collect()).unwrap_or_default();
        if hosts.is_empty() {
            return Err(bad("в QR нет адреса"));
        }
        let hosts: Vec<String> = hosts.into_iter().filter(|h| is_local_address(h)).collect();
        if hosts.is_empty() {
            return Err(bad("в QR-коде адрес не из домашней сети: связь — только между устройствами в одной сети Wi-Fi"));
        }
        let device = fields.get("d").filter(|d| !d.is_empty() && d.len() <= 64).ok_or_else(|| bad("в QR нет устройства"))?.to_string();
        let port = fields.get("p").and_then(|p| p.parse::<u16>().ok()).ok_or_else(|| bad("в QR нет порта"))?;
        let code = normalize_code(fields.get("c").copied().unwrap_or(""))?;
        Ok(Invite { device, name, hosts, port, code })
    }
}

/// Что нужно связи по Wi-Fi в приложении от базы сверх обмена: местные настройки (связанные устройства
/// хранятся там и в обмен не уходят) и число событий в журнале (изменилось — пора обменяться).
pub trait Device: crate::sync::Journal + Send + 'static {
    fn local(&self, key: &str) -> Option<String>;
    fn set_local(&self, key: &str, value: &str) -> Result<()>;
    fn event_count(&self) -> u64;
}

/// Показанный код связи.
#[derive(Clone, Debug)]
pub struct CodeState {
    /// Восемь знаков, без дефиса.
    pub code: String,
    pub until: Instant,
    /// Сколько попыток осталось; 0 — код отменён.
    pub left: u8,
    /// Сколько неверных попыток уже было.
    pub wrong: u8,
}

impl CodeState {
    pub fn new(code: String) -> CodeState {
        CodeState { code, until: Instant::now() + CODE_TTL, left: CODE_TRIES, wrong: 0 }
    }

    /// Код ещё можно ввести.
    pub fn active(&self) -> bool {
        self.left > 0 && Instant::now() < self.until
    }
}

// ——— Ошибки связи ———

/// Почему не получилась связь по Wi-Fi — чтобы приложение могло ответить по делу.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanFail {
    /// На том устройстве код не показан, истёк или уже использован.
    NoCode,
    /// Код не подошёл; сколько попыток осталось.
    WrongCode(u8),
    /// Код отменён после пяти неверных попыток.
    Cancelled,
    /// Устройство не узнало нас: связь отвязана или её не было.
    Unknown,
    /// Устройство занято другим обменом.
    Busy,
    /// Ответило не то устройство: оно не знает кода или ключа пары.
    NotPeer,
}

/// Ошибка связи с понятным текстом.
fn lan_error(f: LanFail, peer: &str) -> Error {
    let who = if peer.is_empty() { "Другое устройство".to_string() } else { format!("«{peer}»") };
    let text = match f {
        LanFail::NoCode => "На том устройстве код не показан или уже не действует: покажите код заново".to_string(),
        LanFail::WrongCode(0) => "Код не подошёл, и он отменён: пять неверных попыток. Покажите на том устройстве новый код".to_string(),
        LanFail::WrongCode(n) => format!("Код не подошёл. Осталось попыток: {n}"),
        LanFail::Cancelled => "Код отменён: было пять неверных попыток. Покажите на том устройстве новый код".to_string(),
        LanFail::Unknown => format!("{who} не узнало это устройство: возможно, там связь отвязали. Свяжите устройства заново"),
        LanFail::Busy => format!("{who} занято другим обменом — попробуйте через минуту"),
        LanFail::NotPeer => "Ответило не то устройство: оно не знает кода или ключа. Проверьте, что выбрано нужное".to_string(),
    };
    Error::Lan(f, text)
}

fn lan_reply_error(v: &Value, peer: &str) -> Option<Error> {
    let err = v["err"].as_str()?;
    let left = v["left"].as_u64().unwrap_or(0).min(CODE_TRIES as u64) as u8;
    Some(lan_error(
        match err {
            "no_code" => LanFail::NoCode,
            "cancelled" => LanFail::Cancelled,
            "wrong_code" => LanFail::WrongCode(left),
            "unknown" => LanFail::Unknown,
            "busy" => LanFail::Busy,
            _ => LanFail::NotPeer,
        },
        peer,
    ))
}

// ——— Сеанс: свои ключи на каждое направление и номер у каждого пакета ———

/// Зашифрованный канал сеанса: XChaCha20-Poly1305, nonce — номер пакета, метка приложения — в AAD.
/// Ключи сеанса не повторяются, поэтому номера с нуля безопасны; пакет не на своём месте
/// не расшифруется.
pub struct SessionWire {
    s: TcpStream,
    tx: XChaCha20Poly1305,
    rx: XChaCha20Poly1305,
    tx_n: u64,
    rx_n: u64,
    aad: [u8; 4],
}

fn nonce(n: u64) -> XNonce {
    let mut b = [0u8; 24];
    b[..8].copy_from_slice(&n.to_le_bytes());
    XNonce::from(b)
}

impl SessionWire {
    /// Канал по готовым ключам: `tx` — для своих пакетов, `rx` — для пакетов другой стороны.
    pub fn new(lan: &Lan, s: TcpStream, tx: [u8; 32], rx: [u8; 32]) -> SessionWire {
        SessionWire { s, tx: XChaCha20Poly1305::new(&tx.into()), rx: XChaCha20Poly1305::new(&rx.into()), tx_n: 0, rx_n: 0, aad: lan.magic }
    }
}

impl Wire for SessionWire {
    fn send_bytes(&mut self, plain: &[u8]) -> Result<()> {
        let ct = self.tx.encrypt(&nonce(self.tx_n), Payload { msg: plain, aad: &self.aad }).map_err(|_| Error::Decrypt)?;
        self.tx_n += 1;
        self.s.write_all(&(ct.len() as u32).to_be_bytes())?;
        self.s.write_all(&ct)?;
        self.s.flush()?;
        Ok(())
    }

    fn recv_bytes(&mut self, max: usize) -> Result<Vec<u8>> {
        let mut len = [0u8; 4];
        self.s.read_exact(&mut len)?;
        let len = u32::from_be_bytes(len) as usize;
        if len > max.min(MAX_FRAME) {
            return Err(Error::Format("слишком большой пакет".into()));
        }
        // Память — по мере прихода данных, а не сразу на объявленную длину: подставной не займёт её словом.
        let mut buf = Vec::new();
        (&mut self.s).take(len as u64).read_to_end(&mut buf)?;
        if buf.len() != len {
            return Err(Error::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
        let plain = self.rx.decrypt(&nonce(self.rx_n), Payload { msg: &buf, aad: &self.aad }).map_err(|_| Error::Decrypt)?;
        self.rx_n += 1;
        Ok(plain)
    }

    fn stream(&self) -> &TcpStream {
        &self.s
    }
}

// ——— Это устройство в сети ———

/// Устройство с другой стороны.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Peer {
    pub device: String,
    pub name: String,
    pub app: String,
}

type KeyFn = dyn Fn(&str) -> Option<[u8; 32]> + Send + Sync;
type PairedFn = dyn Fn(&Peer, &[u8; 32]) -> std::result::Result<(), String> + Send + Sync;
type SyncedFn = dyn Fn(&str, Result<SyncReport>) + Send + Sync;
/// Что сделать, когда оба подтвердили код: запомнить устройство и ключ пары.
pub type OnPaired<'a> = dyn Fn(&Peer, &[u8; 32]) -> std::result::Result<(), String> + 'a;

/// Это устройство в сети: чем отвечать и куда записывать. Общее у сервера и ответчика на поиск.
pub struct Host<J> {
    pub lan: &'static Lan,
    pub store: Arc<Mutex<J>>,
    pub device: String,
    pub name: String,
    /// Показанный код; None — кода нет.
    pub code: Arc<Mutex<Option<CodeState>>>,
    /// Ключ пары по номеру связанного устройства.
    pub peer_key: Arc<KeyFn>,
    /// Связались по коду — запомнить устройство и ключ (до первого обмена). Ошибка — обрыв без обмена.
    pub paired: Arc<PairedFn>,
    /// Обмен закончился — с кем и чем. Ошибка — как есть, чтобы приложение сказало её своими словами.
    pub synced: Arc<SyncedFn>,
}

impl<J> Clone for Host<J> {
    fn clone(&self) -> Self {
        Host {
            lan: self.lan,
            store: self.store.clone(),
            device: self.device.clone(),
            name: self.name.clone(),
            code: self.code.clone(),
            peer_key: self.peer_key.clone(),
            paired: self.paired.clone(),
            synced: self.synced.clone(),
        }
    }
}

impl<J: Journal> Host<J> {
    fn code_active(&self) -> bool {
        lock(&self.code).as_ref().is_some_and(CodeState::active)
    }

    /// Одно подключение: связь по коду или обмен со связанным устройством. Чем закончился обмен —
    /// получает [`Host::synced`]; ошибка здесь — только до обмена.
    pub fn handle(&self, mut s: TcpStream) -> Result<()> {
        s.set_nodelay(true)?;
        s.set_read_timeout(Some(PLAIN_WAIT))?;
        s.set_write_timeout(Some(PLAIN_WAIT))?;
        let (kind, body) = self.lan.read_plain(&mut s)?;
        match kind {
            b'P' => self.host_pair(s, &body),
            b'S' => self.host_sync(s, &body),
            _ => Err(Error::Format("неизвестный запрос".into())),
        }
    }

    fn host_pair(&self, mut s: TcpStream, body: &Value) -> Result<()> {
        let lan = self.lan;
        let a: [u8; 33] = field(body, "a")?;
        // Каждая попытка — минус одна, ещё до проверки: подключение, оборванное после ответа, — тоже попытка.
        let attempt = {
            let mut c = lock(&self.code);
            match c.as_mut() {
                Some(st) if st.active() => {
                    st.left -= 1;
                    Ok((st.code.clone(), st.left))
                }
                Some(st) if st.left == 0 => Err(LanFail::Cancelled),
                _ => Err(LanFail::NoCode),
            }
        };
        let (code, left) = match attempt {
            Ok(x) => x,
            Err(f) => {
                lan.write_plain(&mut s, b'P', &json!({ "err": if f == LanFail::Cancelled { "cancelled" } else { "no_code" } }))?;
                return Err(lan_error(f, ""));
            }
        };
        let (sp, b) = Spake2::<Ed25519Group>::start_b(&Password::new(code.as_bytes()), &Identity::new(lan.joiner), &Identity::new(lan.host));
        let k: [u8; 32] = sp.finish(&a).ok().and_then(|k| k.try_into().ok()).ok_or_else(|| Error::Format("испорченный запрос связи".into()))?;
        lan.write_plain(&mut s, b'P', &json!({ "b": b64(&b), "left": left }))?;
        let (_, body) = lan.read_plain(&mut s)?;
        let confirm: [u8; 32] = field(&body, "confirm")?;
        if !same(&confirm, &lan.mac(&k, &[b"joiner confirm"])) {
            let left = {
                let mut c = lock(&self.code);
                match c.as_mut() {
                    Some(st) => {
                        st.wrong += 1;
                        st.left
                    }
                    None => 0,
                }
            };
            lan.write_plain(&mut s, b'P', &json!({ "err": "wrong_code", "left": left }))?;
            return Err(lan_error(LanFail::WrongCode(left), ""));
        }
        // Код верный — больше не действует.
        lock(&self.code).take();
        lan.write_plain(&mut s, b'P', &json!({ "confirm": b64(&lan.mac(&k, &[b"host confirm"])) }))?;
        let mut w = SessionWire::new(lan, s, lan.mac(&k, &[b"pair s2c"]), lan.mac(&k, &[b"pair c2s"]));
        let peer = match recv_max::<J::Event>(&mut w, PLAIN_MAX)? {
            Msg::Ident { device, name, app } if valid_id(&device) && device != self.device => Peer { device, name: clean_name(&name), app },
            _ => return Err(Error::Format("устройство не назвалось".into())),
        };
        (self.paired)(&peer, &lan.mac(&k, &[b"pair key"])).map_err(Error::Peer)?;
        send::<J::Event>(&mut w, &Msg::Ident { device: self.device.clone(), name: self.name.clone(), app: lan.profile.version.into() })?;
        let r = run_server(lan.profile, &mut w, &self.store, &self.name, Some(&peer.device));
        (self.synced)(&peer.device, r);
        Ok(())
    }

    fn host_sync(&self, mut s: TcpStream, body: &Value) -> Result<()> {
        let lan = self.lan;
        let from = body["from"].as_str().filter(|d| valid_id(d)).ok_or_else(|| Error::Format("нет номера устройства".into()))?.to_string();
        let to = body["to"].as_str().unwrap_or("");
        let nc: [u8; 16] = field(body, "nonce")?;
        if to != self.device {
            lan.write_plain(&mut s, b'S', &json!({ "err": "other" }))?;
            return Err(lan_error(LanFail::NotPeer, ""));
        }
        let Some(key) = (self.peer_key)(&from) else {
            lan.write_plain(&mut s, b'S', &json!({ "err": "unknown" }))?;
            return Err(lan_error(LanFail::Unknown, ""));
        };
        let ns: [u8; 16] = random()?;
        lan.write_plain(&mut s, b'S', &json!({ "nonce": b64(&ns) }))?;
        let mut w = SessionWire::new(lan, s, lan.mac(&key, &[b"sync s2c", &nc, &ns]), lan.mac(&key, &[b"sync c2s", &nc, &ns]));
        let r = run_server(lan.profile, &mut w, &self.store, &self.name, Some(&from));
        (self.synced)(&from, r);
        Ok(())
    }
}

fn connect(addr: SocketAddr) -> Result<TcpStream> {
    let s = TcpStream::connect_timeout(&addr, CONNECT_WAIT)?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(PLAIN_WAIT))?;
    s.set_write_timeout(Some(PLAIN_WAIT))?;
    Ok(s)
}

/// Связь по коду с устройством по адресу: ключ пары и первый обмен. `paired` запоминает устройство
/// сразу, как оба подтвердили код, — до обмена; ошибка в нём — обрыв без обмена.
#[allow(clippy::too_many_arguments)]
pub fn pair_join<J: Journal>(lan: &Lan, addr: SocketAddr, code: &str, store: &Mutex<J>, device: &str, name: &str, paired: &OnPaired) -> Result<(Peer, SyncReport)> {
    let code = normalize_code(code)?;
    let mut s = connect(addr)?;
    let (sp, a) = Spake2::<Ed25519Group>::start_a(&Password::new(code.as_bytes()), &Identity::new(lan.joiner), &Identity::new(lan.host));
    lan.write_plain(&mut s, b'P', &json!({ "a": b64(&a) }))?;
    let (_, r) = lan.read_plain(&mut s)?;
    if let Some(e) = lan_reply_error(&r, "") {
        return Err(e);
    }
    let b: [u8; 33] = field(&r, "b")?;
    let k: [u8; 32] = sp.finish(&b).ok().and_then(|k| k.try_into().ok()).ok_or_else(|| Error::Format("испорченный ответ на связь".into()))?;
    lan.write_plain(&mut s, b'P', &json!({ "confirm": b64(&lan.mac(&k, &[b"joiner confirm"])) }))?;
    let (_, r) = lan.read_plain(&mut s)?;
    if let Some(e) = lan_reply_error(&r, "") {
        return Err(e);
    }
    let confirm: [u8; 32] = field(&r, "confirm")?;
    if !same(&confirm, &lan.mac(&k, &[b"host confirm"])) {
        return Err(lan_error(LanFail::NotPeer, ""));
    }
    let mut w = SessionWire::new(lan, s, lan.mac(&k, &[b"pair c2s"]), lan.mac(&k, &[b"pair s2c"]));
    prepare(w.stream())?;
    send::<J::Event>(&mut w, &Msg::Ident { device: device.into(), name: name.into(), app: lan.profile.version.into() })?;
    let peer = match recv_max::<J::Event>(&mut w, PLAIN_MAX)? {
        Msg::Ident { device: d, name, app } if valid_id(&d) && d != device => Peer { device: d, name: clean_name(&name), app },
        _ => return Err(Error::Format("устройство не назвалось".into())),
    };
    paired(&peer, &lan.mac(&k, &[b"pair key"])).map_err(Error::Peer)?;
    let report = run_client(lan.profile, &mut w, store, name, Some(&peer.device))?;
    Ok((peer, report))
}

/// Обмен со связанным устройством по ключу пары.
#[allow(clippy::too_many_arguments)]
pub fn sync_peer<J: Journal>(lan: &Lan, addr: SocketAddr, me: &str, peer: &str, peer_name: &str, key: &[u8; 32], store: &Mutex<J>, name: &str) -> Result<SyncReport> {
    let mut s = connect(addr)?;
    let nc: [u8; 16] = random()?;
    lan.write_plain(&mut s, b'S', &json!({ "from": me, "to": peer, "nonce": b64(&nc) }))?;
    let (_, r) = lan.read_plain(&mut s)?;
    if let Some(e) = lan_reply_error(&r, peer_name) {
        return Err(e);
    }
    let ns: [u8; 16] = field(&r, "nonce")?;
    let mut w = SessionWire::new(lan, s, lan.mac(key, &[b"sync c2s", &nc, &ns]), lan.mac(key, &[b"sync s2c", &nc, &ns]));
    run_client(lan.profile, &mut w, store, name, Some(peer)).map_err(|e| match e {
        // Ответ не расшифровался — у того, кто ответил, нет ключа пары.
        Error::Decrypt => lan_error(LanFail::NotPeer, peer_name),
        e => e,
    })
}

// ——— Сервер ———

/// Приём подключений в фоне: связь по коду и обмен со связанными устройствами.
pub struct Server {
    pub port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Слушает `port`; занят — любой свободный.
    pub fn start<J: Journal + Send + 'static>(host: Host<J>, port: u16) -> Result<Server> {
        let listener = TcpListener::bind(("0.0.0.0", port)).or_else(|_| TcpListener::bind(("0.0.0.0", 0)))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let busy = Arc::new(AtomicUsize::new(0));
        let thread = std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((s, _)) => {
                        // Лишние подключения — сразу закрыть: чужой в сети не займёт приём.
                        if busy.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                            drop(s);
                            continue;
                        }
                        let _ = s.set_nonblocking(false);
                        busy.fetch_add(1, Ordering::Relaxed);
                        let (host, busy) = (host.clone(), busy.clone());
                        std::thread::spawn(move || {
                            let _ = host.handle(s);
                            busy.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(60)),
                    Err(_) => std::thread::sleep(Duration::from_millis(200)),
                }
            }
        });
        Ok(Server { port, stop, thread: Some(thread) })
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

// ——— Поиск в сети ———

/// Найдено в сети: устройство с показанным кодом или связанное устройство.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Found {
    pub device: String,
    /// Имя — только у устройства с показанным кодом (связанные мы и так знаем).
    pub name: String,
    /// Куда подключаться (TCP).
    pub addr: SocketAddr,
    /// Показан код: с ним можно связаться.
    pub pairing: bool,
    /// Связанное устройство: ответ подписан ключом пары.
    pub peer: bool,
    pub app: String,
}

fn parse_datagram(magic: &[u8; 4], data: &[u8]) -> Option<Value> {
    if data.len() < 5 || data.len() > 1400 || &data[..4] != magic || data[4] != VERSION {
        return None;
    }
    serde_json::from_slice(&data[5..]).ok()
}

/// Ответ на запрос поиска — или ничего (чужому не отвечаем).
fn answer<J: Journal>(host: &Host<J>, tcp_port: u16, q: &Value) -> Option<Value> {
    let lan = host.lan;
    let from = q["from"].as_str().filter(|d| valid_id(d))?;
    if from == host.device {
        return None;
    }
    match q["t"].as_str()? {
        // Устройство без связи ищет, с кем связаться: отвечаем, только пока показан код.
        "pair" if host.code_active() => Some(json!({ "t": "pair", "d": host.device, "name": host.name, "p": tcp_port, "app": lan.profile.version })),
        // Связанное устройство: запрос подписан ключом пары — и ответ тоже.
        "peer" => {
            if q["to"].as_str()? != host.device {
                return None;
            }
            let key = (host.peer_key)(from)?;
            let n: [u8; 16] = field(q, "n").ok()?;
            let m: [u8; 32] = field(q, "m").ok()?;
            if !same(&m, &lan.mac(&key, &[b"query", from.as_bytes(), host.device.as_bytes(), &n])) {
                return None;
            }
            let port = tcp_port.to_string();
            let am = lan.mac(&key, &[b"answer", host.device.as_bytes(), from.as_bytes(), &n, port.as_bytes()]);
            Some(json!({ "t": "peer", "d": host.device, "p": tcp_port, "app": lan.profile.version, "m": b64(&am) }))
        }
        _ => None,
    }
}

/// Отвечает на поиск в сети, пока не остановят.
pub struct Responder {
    pub port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Responder {
    /// Слушает `port` (UDP); `tcp_port` — порт сервера для ответа.
    pub fn start<J: Journal + Send + 'static>(host: Host<J>, port: u16, tcp_port: u16) -> Result<Responder> {
        let sock = UdpSocket::bind(("0.0.0.0", port))?;
        let port = sock.local_addr()?.port();
        sock.set_read_timeout(Some(Duration::from_millis(250)))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            let (mut second, mut seen) = (Instant::now(), 0usize);
            while !stop2.load(Ordering::Relaxed) {
                let Ok((n, src)) = sock.recv_from(&mut buf) else { continue };
                if second.elapsed() >= Duration::from_secs(1) {
                    (second, seen) = (Instant::now(), 0);
                }
                // Поток запросов (с чужими подписями — на каждую проверку нужен ключ пары) не разбираем.
                seen += 1;
                if seen > MAX_QUERIES {
                    continue;
                }
                let Some(q) = parse_datagram(&host.lan.query_magic, &buf[..n]) else { continue };
                if let Some(a) = answer(&host, tcp_port, &q) {
                    let _ = sock.send_to(&host.lan.datagram(&host.lan.answer_magic, &a), src);
                }
            }
        });
        Ok(Responder { port, stop, thread: Some(thread) })
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Ищет в сети устройства с показанным кодом и связанные устройства (`peers` — номера и ключи пары).
/// `targets` — куда слать запрос: широковещательные адреса сетей и порт поиска.
pub fn discover(lan: &Lan, targets: &[SocketAddr], me: &str, peers: &[(String, [u8; 32])], wait: Duration) -> Result<Vec<Found>> {
    let sock = UdpSocket::bind(("0.0.0.0", 0))?;
    sock.set_broadcast(true)?;
    let mut nonces: BTreeMap<String, ([u8; 16], [u8; 32])> = BTreeMap::new();
    let mut packets = vec![lan.datagram(&lan.query_magic, &json!({ "t": "pair", "from": me }))];
    for (id, key) in peers {
        let n: [u8; 16] = random()?;
        let m = lan.mac(key, &[b"query", me.as_bytes(), id.as_bytes(), &n]);
        packets.push(lan.datagram(&lan.query_magic, &json!({ "t": "peer", "from": me, "to": id, "n": b64(&n), "m": b64(&m) })));
        nonces.insert(id.clone(), (n, *key));
    }
    for t in targets {
        for p in &packets {
            // Сеть без широковещания или адрес недоступен — пробуем остальные.
            let _ = sock.send_to(p, t);
        }
    }
    let until = Instant::now() + wait;
    let mut found: BTreeMap<String, Found> = BTreeMap::new();
    let mut buf = [0u8; 1500];
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        sock.set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
        let Ok((n, src)) = sock.recv_from(&mut buf) else { continue };
        let Some(a) = parse_datagram(&lan.answer_magic, &buf[..n]) else { continue };
        let (Some(d), Some(p)) = (a["d"].as_str().filter(|d| valid_id(d) && *d != me), a["p"].as_u64().and_then(|p| u16::try_from(p).ok())) else { continue };
        let addr = SocketAddr::new(answer_ip(src.ip()), p);
        let app = a["app"].as_str().unwrap_or("").chars().take(16).collect::<String>();
        match a["t"].as_str() {
            Some("pair") => {
                let e = found.entry(d.to_string()).or_insert_with(|| Found { device: d.into(), name: String::new(), addr, pairing: true, peer: false, app: app.clone() });
                e.pairing = true;
                e.name = clean_name(a["name"].as_str().unwrap_or(""));
            }
            Some("peer") => {
                let Some((nq, key)) = nonces.get(d) else { continue };
                let Ok(m) = field::<32>(&a, "m") else { continue };
                if !same(&m, &lan.mac(key, &[b"answer", d.as_bytes(), me.as_bytes(), nq, p.to_string().as_bytes()])) {
                    continue;
                }
                let e = found.entry(d.to_string()).or_insert_with(|| Found { device: d.into(), name: String::new(), addr, pairing: false, peer: true, app: app.clone() });
                e.peer = true;
                e.addr = addr;
            }
            _ => {}
        }
    }
    Ok(found.into_values().collect())
}

/// Адрес ответившего: IPv4 из IPv6-обёртки — как есть.
fn answer_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

#[cfg(test)]
mod tests;
