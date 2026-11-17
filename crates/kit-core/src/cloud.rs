//! Облако: Яндекс Диск, свой сервер WebDAV или Google Drive — общий ящик для устройств одного человека.
//!
//! Устроено как в «Кармане» 0.15 и «Полке» 0.11: тот же формат, у каждого приложения свои имена и метки
//! ([`Format`]). Менять их нельзя — иначе устройства перестанут читать то, что уже лежит в облаке.
//!
//! Облако не знает, что лежит в файлах: всё шифруется на устройстве до отправки.
//! - `<приложение>.key` — ключ облака (случайный, 256 бит), зашифрованный Argon2id от пароля копий.
//!   По паролю новое устройство получает ключ облака; сам пароль нигде не хранится.
//! - `e/<устройство>-<с>-<по>.<расширение>` — события одного устройства с номерами «с…по», сжатые
//!   и зашифрованные XChaCha20-Poly1305. Имя файла входит в проверку подлинности:
//!   переименованный или подменённый файл не расшифруется.
//! - `b/<имя>.<расширение>` — вложение (снимок чека, обложка). Имя — MAC от отпечатка: по имени
//!   нельзя проверить, лежит ли в облаке известное вложение.
//! - `k/<имя>-<кусок>-<всего>.<расширение>` — большой файл кусками (у «Полки» — файлы книг), если
//!   у приложения они есть и обмен ими включён. Имя — MAC от отпечатка файла; номер куска и их число
//!   входят в проверку подлинности. Файл забирается, когда в облаке лежат все его куски.
//!
//! Общего состояния у устройств нет: по именам файлов видно, какие события есть в облаке.
//! Устройство забирает файлы, которые продолжают его журнал без пропусков, и отправляет то,
//! чего в облаке нет, — в том числе события других устройств, полученные по Wi-Fi.
//! Мелкие файлы одного устройства время от времени сливаются в крупные.

use crate::backup::{self, Kdf};
use crate::error::{Error, Result};
use crate::sync::{FileInfo, Journal};
use crate::AppNames;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use blake2::digest::consts::U32;
use blake2::digest::Mac;
use blake2::Blake2bMac;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

const KEY_FORMAT: u16 = 1;
const FILE_VERSION: u8 = 1;
const KIND_EVENTS: u8 = b'e';
const KIND_BLOB: u8 = b'b';
const KIND_FILE: u8 = b'k';
/// Заголовок зашифрованного файла: метка, версия, вид, nonce и метка подлинности.
pub const OVERHEAD: usize = 4 + 1 + 1 + 24 + 16;
/// Событий в одном файле — не больше.
pub const FILE_EVENTS: u64 = 5000;
/// Сколько мелких файлов одного устройства копится до слияния.
pub const COMPACT_AFTER: usize = 12;
/// Предел файла событий в облаке.
pub const MAX_EVENTS_FILE: usize = 64 << 20;
const MAX_PLAIN: usize = 256 << 20;
/// Предел файла ключа.
pub const MAX_KEY_FILE: usize = 64 << 10;
/// Полный список вложений в облаке сверяем не чаще раза в сутки, если нет новых.
const BLOBS_RELIST: i64 = 24 * 3600;

/// Что случилось с облаком — от этого зависит подсказка на экране.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudFail {
    /// Логин или пароль не подходит, вход устарел или доступ отозван — войти заново.
    Auth,
    /// На диске нет места.
    Space,
    /// Нет связи с облаком.
    Offline,
    /// Облако ответило ошибкой — обычно временной.
    Server,
    /// Данные приложения в облаке удалены.
    Gone,
    /// Облако подключили заново, с другим ключом.
    Replaced,
    /// Записи этого устройства в облаке расходятся с базой.
    Conflict,
    /// Файл записан более новой версией приложения.
    Newer,
    /// Файл или ответ облака не разобрать.
    Format,
    /// Обмен остановлен.
    Cancelled,
}

/// Вложения приложения в облаке: как называются в сообщениях и какие подходят.
#[derive(Clone, Copy, Debug)]
pub struct Blobs {
    /// Расширение файла: `.kbl`.
    pub ext: &'static str,
    /// Предел файла в облаке: вложение и немного на заголовок.
    pub max_file: usize,
    /// Именительный падеж: «снимок чека», «обложка».
    pub what: &'static str,
    /// Для подсказки, где взять: «этот снимок», «эта обложка».
    pub this: &'static str,
    /// Родительный падеж: «снимка», «обложки» (неверный номер снимка).
    pub of: &'static str,
    /// Женский род: «обложка повреждена»; иначе — «снимок повреждён».
    pub feminine: bool,
    /// Подходит ли вложение для базы: тип, размер, первые байты.
    pub check: fn(&str, &[u8]) -> bool,
}

/// Большие файлы приложения в облаке (у «Полки» — файлы книг), кусками.
#[derive(Clone, Copy, Debug)]
pub struct Files {
    /// Расширение куска: `.pbk`.
    pub ext: &'static str,
    /// Размер куска — как у [`Journal::file_read`].
    pub part: usize,
    /// Как назвать файл в сообщении: «Файл книги».
    pub what: &'static str,
}

/// Облако одного приложения: имена, метки и что в нём лежит.
#[derive(Clone, Copy, Debug)]
pub struct Format {
    /// Название — для сообщений; `names.id` — в строках ключей (`karman-cloud`).
    pub names: AppNames,
    /// Файл ключа облака: `karman.key`.
    pub key_file: &'static str,
    /// Метка в начале каждого зашифрованного файла: `KRMC`.
    pub magic: [u8; 4],
    /// Расширение файлов событий: `.kev`.
    pub events_ext: &'static str,
    /// Самый большой номер события в имени файла (у «Полки» номера в базе — i64).
    pub max_seq: u64,
    pub blobs: Blobs,
    /// Большие файлы кусками; `None` — их нет, папки `k` тоже.
    pub files: Option<Files>,
}

impl Format {
    /// Папки с файлами приложения (кроме самой папки приложения).
    pub fn dirs(&self) -> Vec<Dir> {
        let mut d = vec![Dir::Events, Dir::Blobs];
        if self.files.is_some() {
            d.push(Dir::Files);
        }
        d
    }

    fn gone(&self) -> Error {
        fail(CloudFail::Gone, format!("Данные «{}» в облаке удалены. Чтобы продолжить обмен, подключите облако заново", self.names.name_gen))
    }

    /// MAC и вывод ключей: BLAKE2b с ключом и личной строкой приложения (`karman-cloud`).
    fn mac256(&self, key: &[u8], data: &[u8]) -> [u8; 32] {
        let personal = format!("{}-cloud", self.names.id);
        let mut mac = Blake2bMac::<U32>::new_with_salt_and_personal(key, &[], personal.as_bytes()).expect("ключ MAC — 32 байта");
        mac.update(data);
        mac.finalize().into_bytes().into()
    }

    /// Предел куска большого файла в облаке.
    fn max_file_part(&self) -> usize {
        self.files.map(|f| f.part + OVERHEAD + 64).unwrap_or(0)
    }
}

fn fail(kind: CloudFail, msg: impl Into<String>) -> Error {
    Error::Cloud(kind, msg.into())
}

fn replaced() -> Error {
    fail(CloudFail::Replaced, "Облако подключили заново на другом устройстве. Введите пароль копий, чтобы продолжить обмен")
}

fn conflict() -> Error {
    fail(
        CloudFail::Conflict,
        "В облаке записи этого устройства расходятся с базой: его база начиналась заново под тем же номером. Обмен остановлен, чтобы ничего не потерять",
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Номер вложения — SHA-256 содержимого, шестнадцатеричными знаками.
pub fn blob_id(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn unhex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn valid_group(g: &str) -> bool {
    g.len() == 32 && g.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Папка внутри папки приложения в облаке.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dir {
    Root,
    Events,
    Blobs,
    /// Большие файлы кусками (у «Полки» — файлы книг).
    Files,
}

impl Dir {
    /// Имя папки: пусто — сама папка приложения.
    pub fn folder(self) -> &'static str {
        match self {
            Dir::Root => "",
            Dir::Events => "e",
            Dir::Blobs => "b",
            Dir::Files => "k",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
}

/// Облачное хранилище: папка приложения с подпапками для событий, вложений и больших файлов.
pub trait Cloud {
    /// Файлы в папке; если папки нет — пустой список.
    fn list(&self, dir: Dir) -> Result<Vec<Entry>>;
    /// Содержимое файла не больше `max` байт; `None` — файла нет.
    fn get(&self, dir: Dir, name: &str, max: usize) -> Result<Option<Vec<u8>>>;
    /// Записывает файл целиком: новый или поверх прежнего.
    fn put(&self, dir: Dir, name: &str, data: &[u8]) -> Result<()>;
    /// Файл есть.
    fn exists(&self, dir: Dir, name: &str) -> Result<bool>;
    /// Удаляет файл; если его уже нет — не ошибка.
    fn delete(&self, dir: Dir, name: &str) -> Result<()>;
}

/// Чего обмену с облаком нужно от базы сверх обмена между устройствами ([`Journal`]).
pub trait CloudStore: Journal {
    /// Устройство и номер события.
    fn origin(e: &Self::Event) -> (&str, u64);
    /// События устройства `author` с номерами `from..=to`, по порядку.
    fn events_range(&self, author: &str, from: u64, to: u64) -> Result<Vec<Self::Event>>;
    /// Событие с тем же устройством и номером уже записано, и записано иначе.
    fn differs(&self, e: &Self::Event) -> Result<bool>;
    /// Вложения, которые есть в базе.
    fn present_blobs(&self) -> Result<Vec<String>>;
}

// ——— Ключ облака ———

/// Файл ключа в облаке: параметры Argon2id, соль и зашифрованный ключ облака.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyFile {
    pub format: u16,
    /// Номер облака: меняется, только если облако подключают заново с нуля.
    pub group: String,
    pub m: u32,
    pub t: u32,
    pub p: u32,
    pub salt: String,
    pub nonce: String,
    pub key: String,
    /// Когда задан пароль: секунды Unix.
    #[serde(default)]
    pub changed: i64,
}

impl KeyFile {
    pub fn parse(fmt: &Format, data: &[u8]) -> Result<KeyFile> {
        let kf: KeyFile = serde_json::from_slice(data).map_err(|_| fail(CloudFail::Format, "Ключ облака повреждён"))?;
        if kf.format > KEY_FORMAT {
            return Err(fail(CloudFail::Newer, format!("Облако подключено более новой версией «{}» — обновите приложение", fmt.names.name_gen)));
        }
        if kf.format == 0 || !valid_group(&kf.group) {
            return Err(fail(CloudFail::Format, "Ключ облака повреждён"));
        }
        Ok(kf)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).unwrap_or_default()
    }

    fn kdf(&self) -> Kdf {
        Kdf { m_kib: self.m, t: self.t, p: self.p }
    }

    /// Параметры и номер облака защищены вместе с ключом: подменить их незаметно нельзя.
    fn aad(&self, fmt: &Format) -> Vec<u8> {
        format!("{}-cloud-key/{}|{}|{}|{}|{}|{}", fmt.names.id, self.format, self.group, self.m, self.t, self.p, self.salt).into_bytes()
    }
}

/// Ключ облака. Хранится в базе устройства (она сама зашифрована) — для обмена пароль не нужен.
pub struct CloudKey {
    fmt: &'static Format,
    group: String,
    key: [u8; 32],
}

impl Drop for CloudKey {
    fn drop(&mut self) {
        backup::wipe(&mut self.key);
    }
}

impl CloudKey {
    /// Новый ключ для облака, которое подключают впервые.
    pub fn generate(fmt: &'static Format) -> Result<CloudKey> {
        let group: [u8; 16] = backup::random()?;
        Ok(CloudKey { fmt, group: hex(&group), key: backup::random()? })
    }

    pub fn group(&self) -> &str {
        &self.group
    }

    /// Для хранения в базе: «1:номер облака:ключ».
    pub fn to_meta(&self) -> String {
        format!("1:{}:{}", self.group, B64.encode(self.key))
    }

    pub fn from_meta(fmt: &'static Format, s: &str) -> Result<CloudKey> {
        let bad = || fail(CloudFail::Format, "Ключ облака на устройстве повреждён: подключите облако заново");
        let p: Vec<&str> = s.split(':').collect();
        if p.len() != 3 || p[0] != "1" || !valid_group(p[1]) {
            return Err(bad());
        }
        let mut raw = B64.decode(p[2]).map_err(|_| bad())?;
        let key = <[u8; 32]>::try_from(raw.as_slice()).map_err(|_| bad());
        backup::wipe(&mut raw);
        Ok(CloudKey { fmt, group: p[1].to_string(), key: key? })
    }

    /// Файл ключа для облака: ключ облака, зашифрованный паролем копий (около секунды).
    pub fn key_file(&self, password: &str, kdf: Kdf, changed: i64) -> Result<KeyFile> {
        backup::check_password(password)?;
        if !kdf.valid() {
            return Err(fail(CloudFail::Format, "неподходящие параметры ключа"));
        }
        let salt: [u8; 16] = backup::random()?;
        let nonce: [u8; 24] = backup::random()?;
        let mut kf = KeyFile {
            format: KEY_FORMAT,
            group: self.group.clone(),
            m: kdf.m_kib,
            t: kdf.t,
            p: kdf.p,
            salt: B64.encode(salt),
            nonce: B64.encode(nonce),
            key: String::new(),
            changed,
        };
        let mut k = backup::derive_raw(password, &salt, kdf)?;
        let wrapped = XChaCha20Poly1305::new((&k).into()).encrypt(XNonce::from_slice(&nonce), Payload { msg: &self.key, aad: &kf.aad(self.fmt) });
        backup::wipe(&mut k);
        kf.key = B64.encode(wrapped.map_err(|_| Error::Decrypt)?);
        Ok(kf)
    }

    /// Ключ облака из файла ключа по паролю копий (около секунды).
    pub fn from_key_file(fmt: &'static Format, kf: &KeyFile, password: &str) -> Result<CloudKey> {
        let bad = || fail(CloudFail::Format, "Ключ облака повреждён");
        if !kf.kdf().valid() {
            return Err(fail(CloudFail::Format, "Ключ облака не открывается: неподходящие параметры"));
        }
        let salt = <[u8; 16]>::try_from(B64.decode(&kf.salt).map_err(|_| bad())?.as_slice()).map_err(|_| bad())?;
        let nonce = <[u8; 24]>::try_from(B64.decode(&kf.nonce).map_err(|_| bad())?.as_slice()).map_err(|_| bad())?;
        let wrapped = B64.decode(&kf.key).map_err(|_| bad())?;
        if wrapped.len() != 32 + 16 {
            return Err(bad());
        }
        let mut k = backup::derive_raw(password, &salt, kf.kdf())?;
        let opened = XChaCha20Poly1305::new((&k).into()).decrypt(XNonce::from_slice(&nonce), Payload { msg: &wrapped, aad: &kf.aad(fmt) });
        backup::wipe(&mut k);
        let mut raw = opened.map_err(|_| Error::CloudPassword)?;
        let key = <[u8; 32]>::try_from(raw.as_slice()).map_err(|_| Error::CloudPassword);
        backup::wipe(&mut raw);
        Ok(CloudKey { fmt, group: kf.group.clone(), key: key? })
    }

    /// Свой ключ для каждой задачи: шифрование файлов и имена вложений.
    fn sub(&self, label: &str) -> [u8; 32] {
        self.fmt.mac256(&self.key, label.as_bytes())
    }

    /// Имя файла вложения в облаке: по нему не узнать отпечаток.
    pub fn blob_name(&self, id: &str) -> String {
        let mut k = self.sub("names");
        let name = self.fmt.mac256(&k, id.as_bytes());
        backup::wipe(&mut k);
        format!("{}{}", &hex(&name)[..32], self.fmt.blobs.ext)
    }

    /// Имя куска большого файла в облаке: по нему не узнать отпечаток файла.
    pub fn file_name(&self, id: &str, part: u64, parts: u64) -> String {
        let mut k = self.sub("books");
        let name = self.fmt.mac256(&k, id.as_bytes());
        backup::wipe(&mut k);
        format!("{}-{part}-{parts}{}", &hex(&name)[..32], self.fmt.files.map(|f| f.ext).unwrap_or(""))
    }
}

fn hex32(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Кусок большого файла: имя, номер куска, всего кусков.
fn parse_file_name<'a>(fmt: &Format, name: &'a str) -> Option<(&'a str, u64, u64)> {
    let mut it = name.strip_suffix(fmt.files?.ext)?.split('-');
    let (mac, part, parts) = (it.next()?, it.next()?.parse::<u64>().ok()?, it.next()?.parse::<u64>().ok()?);
    (it.next().is_none() && hex32(mac) && part < parts && parts <= 64).then_some((mac, part, parts))
}

pub fn read_key_file(fmt: &Format, cloud: &dyn Cloud) -> Result<Option<KeyFile>> {
    cloud.get(Dir::Root, fmt.key_file, MAX_KEY_FILE)?.map(|b| KeyFile::parse(fmt, &b)).transpose()
}

/// Файл приложения в его папке облака. Удаляются только такие: чужие файлы в «e» и «b»
/// (если человек положил их туда сам) остаются.
fn ours(fmt: &Format, dir: Dir, name: &str) -> bool {
    match dir {
        Dir::Root => name == fmt.key_file,
        Dir::Events => parse_event_name(fmt, name).is_some(),
        Dir::Blobs => name.strip_suffix(fmt.blobs.ext).is_some_and(hex32),
        Dir::Files => parse_file_name(fmt, name).is_some(),
    }
}

/// Подключает устройство по паролю копий: ключ облака уже есть — открывает его, нет — создаёт.
/// Возвращает ключ и `true`, если облако подключено впервые.
pub fn join(fmt: &'static Format, cloud: &dyn Cloud, password: &str, kdf: Kdf, now: i64) -> Result<(CloudKey, bool)> {
    backup::check_password(password)?;
    if let Some(kf) = read_key_file(fmt, cloud)? {
        return Ok((CloudKey::from_key_file(fmt, &kf, password)?, false));
    }
    // Без ключа старые файлы приложения не прочитать никому: убираем их, чтобы не мешали.
    for dir in fmt.dirs() {
        for e in cloud.list(dir)?.into_iter().filter(|e| ours(fmt, dir, &e.name)) {
            cloud.delete(dir, &e.name)?;
        }
    }
    let key = CloudKey::generate(fmt)?;
    let kf = key.key_file(password, kdf, now)?;
    cloud.put(Dir::Root, fmt.key_file, &kf.to_bytes())?;
    // Если в ту же минуту облако подключило другое устройство, действует записанный последним ключ.
    match read_key_file(fmt, cloud)? {
        Some(cur) if cur.group == key.group => Ok((key, true)),
        Some(cur) => Ok((CloudKey::from_key_file(fmt, &cur, password)?, false)),
        None => Err(fmt.gone()),
    }
}

/// Новый пароль копий: тот же ключ облака, зашифрованный новым паролем.
pub fn put_key_file(cloud: &dyn Cloud, key: &CloudKey, kf: &KeyFile) -> Result<()> {
    if kf.group != key.group {
        return Err(replaced());
    }
    match read_key_file(key.fmt, cloud)? {
        None => Err(key.fmt.gone()),
        Some(cur) if cur.group != key.group => Err(replaced()),
        Some(_) => cloud.put(Dir::Root, key.fmt.key_file, &kf.to_bytes()),
    }
}

/// Удаляет данные приложения из облака. Сначала ключ: другие устройства увидят это и перестанут слать файлы.
pub fn wipe(fmt: &Format, cloud: &dyn Cloud, cancel: &AtomicBool) -> Result<usize> {
    cloud.delete(Dir::Root, fmt.key_file)?;
    let mut n = 1;
    for dir in fmt.dirs() {
        for e in cloud.list(dir)?.into_iter().filter(|e| ours(fmt, dir, &e.name)) {
            if cancel.load(Ordering::Relaxed) {
                return Err(fail(CloudFail::Cancelled, "Удаление остановлено"));
            }
            cloud.delete(dir, &e.name)?;
            n += 1;
        }
    }
    Ok(n)
}

// ——— Файлы ———

fn aad(fmt: &Format, kind: u8, group: &str, name: &str) -> Vec<u8> {
    format!("{}-cloud/1|{}|{group}|{name}", fmt.names.id, kind as char).into_bytes()
}

fn seal_file(key: &CloudKey, kind: u8, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
    let nonce: [u8; 24] = backup::random()?;
    let mut k = key.sub("files");
    let ct = XChaCha20Poly1305::new((&k).into()).encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad: &aad(key.fmt, kind, &key.group, name) });
    backup::wipe(&mut k);
    let ct = ct.map_err(|_| Error::Decrypt)?;
    let mut out = Vec::with_capacity(OVERHEAD + plain.len());
    out.extend_from_slice(&key.fmt.magic);
    out.push(FILE_VERSION);
    out.push(kind);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn open_file(key: &CloudKey, kind: u8, name: &str, data: &[u8]) -> Result<Vec<u8>> {
    let fmt = key.fmt;
    let (what, feminine) = if kind == KIND_BLOB { (fmt.blobs.what, fmt.blobs.feminine) } else { ("файл", false) };
    let damaged = if feminine { "повреждена" } else { "повреждён" };
    if data.len() < OVERHEAD || data[..4] != fmt.magic || data[5] != kind {
        return Err(fail(CloudFail::Format, format!("{what} в облаке не от «{}» или {damaged} ({name})", fmt.names.name_gen)));
    }
    if data[4] > FILE_VERSION {
        return Err(fail(CloudFail::Newer, format!("В облаке файлы более новой версии «{}» — обновите приложение", fmt.names.name_gen)));
    }
    let mut k = key.sub("files");
    let plain = XChaCha20Poly1305::new((&k).into()).decrypt(XNonce::from_slice(&data[6..30]), Payload { msg: &data[30..], aad: &aad(fmt, kind, &key.group, name) });
    backup::wipe(&mut k);
    let swapped = if feminine { "повреждена или подменена" } else { "повреждён или подменён" };
    plain.map_err(|_| fail(CloudFail::Format, format!("{what} в облаке {swapped} ({name})")))
}

/// Имя файла событий: устройство и номера событий «с…по».
pub fn event_name(fmt: &Format, author: &str, from: u64, to: u64) -> String {
    format!("{author}-{from}-{to}{}", fmt.events_ext)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventFileName {
    pub author: String,
    pub from: u64,
    pub to: u64,
}

/// Разбирает имя файла событий. Чужие и неканонические имена («007» вместо «7») не подходят.
pub fn parse_event_name(fmt: &Format, name: &str) -> Option<EventFileName> {
    let stem = name.strip_suffix(fmt.events_ext)?;
    let mut parts = stem.split('-');
    let author = parts.next()?;
    let from: u64 = parts.next()?.parse().ok()?;
    let to: u64 = parts.next()?.parse().ok()?;
    let ok_author = !author.is_empty() && author.len() <= 64 && author.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if parts.next().is_some() || !ok_author || from == 0 || to < from || to > fmt.max_seq || event_name(fmt, author, from, to) != name {
        return None;
    }
    Some(EventFileName { author: author.to_string(), from, to })
}

#[derive(Serialize)]
struct EventFileOut<'a, E> {
    author: &'a str,
    from: u64,
    to: u64,
    events: &'a [E],
}

#[derive(Deserialize)]
struct EventFileIn<E> {
    author: String,
    from: u64,
    to: u64,
    events: Vec<E>,
}

fn seal_events<E: Serialize>(key: &CloudKey, author: &str, from: u64, to: u64, events: &[E]) -> Result<(String, Vec<u8>)> {
    let name = event_name(key.fmt, author, from, to);
    let json = serde_json::to_vec(&EventFileOut { author, from, to, events })?;
    let packed = miniz_oxide::deflate::compress_to_vec(&json, 6);
    let data = seal_file(key, KIND_EVENTS, &name, &packed)?;
    Ok((name, data))
}

/// Расшифровывает файл событий и проверяет, что внутри ровно то, что обещает имя.
fn open_events<J: CloudStore>(key: &CloudKey, author: &str, from: u64, to: u64, data: &[u8]) -> Result<Vec<J::Event>> {
    let name = event_name(key.fmt, author, from, to);
    let bad = || fail(CloudFail::Format, format!("файл в облаке повреждён ({name})"));
    let packed = open_file(key, KIND_EVENTS, &name, data)?;
    let json = miniz_oxide::inflate::decompress_to_vec_with_limit(&packed, MAX_PLAIN).map_err(|_| bad())?;
    let f: EventFileIn<J::Event> = serde_json::from_slice(&json).map_err(|_| bad())?;
    if f.author != author || f.from != from || f.to != to || f.events.len() as u64 != to - from + 1 {
        return Err(bad());
    }
    if f.events.iter().enumerate().any(|(i, e)| J::origin(e) != (author, from + i as u64)) {
        return Err(bad());
    }
    Ok(f.events)
}

fn seal_blob(key: &CloudKey, id: &str, mime: &str, data: &[u8]) -> Result<(String, Vec<u8>)> {
    let raw_id = unhex32(id).ok_or_else(|| fail(CloudFail::Format, format!("неверный номер {}", key.fmt.blobs.of)))?;
    if mime.is_empty() || mime.len() > 255 {
        return Err(fail(CloudFail::Format, format!("неверный тип {}", key.fmt.blobs.of)));
    }
    let mut plain = Vec::with_capacity(1 + mime.len() + 32 + data.len());
    plain.push(mime.len() as u8);
    plain.extend_from_slice(mime.as_bytes());
    plain.extend_from_slice(&raw_id);
    plain.extend_from_slice(data);
    let name = key.blob_name(id);
    let sealed = seal_file(key, KIND_BLOB, &name, &plain)?;
    Ok((name, sealed))
}

/// Вложение из облака: тип и содержимое, отпечаток которого совпал с ожидаемым номером.
fn open_blob(key: &CloudKey, id: &str, name: &str, data: &[u8]) -> Result<(String, Vec<u8>)> {
    let b = &key.fmt.blobs;
    let bad = || fail(CloudFail::Format, format!("{} в облаке {} ({name})", b.what, if b.feminine { "повреждена" } else { "повреждён" }));
    let plain = open_file(key, KIND_BLOB, name, data)?;
    let mime_len = *plain.first().ok_or_else(bad)? as usize;
    let mime = std::str::from_utf8(plain.get(1..1 + mime_len).ok_or_else(bad)?).map_err(|_| bad())?.to_string();
    let raw_id = plain.get(1 + mime_len..1 + mime_len + 32).ok_or_else(bad)?;
    let body = plain[1 + mime_len + 32..].to_vec();
    if hex(raw_id) != id || blob_id(&body) != id || !(b.check)(&mime, &body) {
        return Err(bad());
    }
    Ok((mime, body))
}

// ——— Обмен ———

/// Что устройство помнит об облаке между обменами (хранится в базе устройства).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Файлы этого устройства в облаке, сверенные с базой.
    #[serde(default)]
    pub checked: BTreeSet<String>,
    /// Вложения, которые точно есть в облаке (имена файлов).
    #[serde(default)]
    pub blobs: BTreeSet<String>,
    /// Когда список вложений в облаке сверяли целиком: секунды Unix.
    #[serde(default)]
    pub blobs_listed: i64,
    /// Обмен уже проходил хотя бы раз.
    #[serde(default)]
    pub synced: bool,
}

/// Один обмен с облаком.
pub struct Round<'a, J> {
    pub cloud: &'a dyn Cloud,
    pub store: &'a Mutex<J>,
    pub key: &'a CloudKey,
    /// Сейчас: секунды Unix.
    pub now: i64,
    /// Остановить обмен между запросами.
    pub cancel: &'a AtomicBool,
}

/// Чем занят обмен — для экрана.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    Events,
    Upload { done: usize, total: usize },
    Download { done: usize, total: usize },
    /// Большие файлы: отправка и получение.
    Files { done: usize, total: usize },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// Новых событий пришло.
    pub received: usize,
    /// Событий отправлено в облако.
    pub sent: usize,
    /// Вложений пришло и ушло.
    pub files_received: usize,
    pub files_sent: usize,
    /// Вложений, которых в облаке пока нет: их ещё не отправило другое устройство.
    pub files_waiting: usize,
    /// Больших файлов пришло и ушло целиком; сколько ещё не лежит в облаке целиком.
    #[serde(default)]
    pub whole_received: usize,
    #[serde(default)]
    pub whole_sent: usize,
    #[serde(default)]
    pub whole_waiting: usize,
    /// Что не получилось, но обмену не помешало.
    pub problems: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    from: u64,
    to: u64,
}

impl Span {
    fn name(self, fmt: &Format, author: &str) -> String {
        event_name(fmt, author, self.from, self.to)
    }
}

enum Fetched<E> {
    Events(Vec<E>),
    /// Файл пропал между списком и скачиванием (его слило другое устройство).
    Vanished,
    Broken(String),
}

fn lock<J>(store: &Mutex<J>) -> MutexGuard<'_, J> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

/// Последний номер события каждого устройства в базе.
fn known<J: Journal>(store: &Mutex<J>) -> Result<BTreeMap<String, u64>> {
    lock(store).versions()
}

/// Отрезки 1..=max, не покрытые файлами.
fn gaps(spans: &[Span], max: u64) -> Vec<(u64, u64)> {
    let mut sorted = spans.to_vec();
    sorted.sort_by_key(|s| s.from);
    let mut out = Vec::new();
    let mut next = 1u64;
    for s in sorted {
        if s.from > next {
            out.push((next, (s.from - 1).min(max)));
        }
        next = next.max(s.to.saturating_add(1));
        if next > max {
            break;
        }
    }
    if next <= max {
        out.push((next, max));
    }
    out.retain(|(a, b)| a <= b);
    out
}

/// Отрезок, нарезанный на куски по FILE_EVENTS — одинаково на всех устройствах.
fn chunks(from: u64, to: u64) -> Vec<Span> {
    let mut out = Vec::new();
    let mut a = from;
    while a <= to {
        let b = to.min(a + FILE_EVENTS - 1);
        out.push(Span { from: a, to: b });
        a = b + 1;
    }
    out
}

impl<J: CloudStore> Round<'_, J> {
    fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(fail(CloudFail::Cancelled, "Обмен остановлен"))
        } else {
            Ok(())
        }
    }

    /// Облако то же, к которому подключались: ключ на месте и с тем же номером.
    fn same_cloud(&self) -> Result<()> {
        match read_key_file(self.key.fmt, self.cloud)? {
            None => Err(self.key.fmt.gone()),
            Some(kf) if kf.group != self.key.group => Err(replaced()),
            Some(_) => Ok(()),
        }
    }

    fn list_events(&self) -> Result<BTreeMap<String, Vec<Span>>> {
        let mut out: BTreeMap<String, Vec<Span>> = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for e in self.cloud.list(Dir::Events)? {
            if !seen.insert(e.name.clone()) {
                continue;
            }
            if let Some(f) = parse_event_name(self.key.fmt, &e.name) {
                out.entry(f.author).or_default().push(Span { from: f.from, to: f.to });
            }
        }
        Ok(out)
    }

    fn events_range(&self, author: &str, span: Span) -> Result<Vec<J::Event>> {
        lock(self.store).events_range(author, span.from, span.to)
    }

    fn fetch_once(&self, author: &str, span: Span) -> Result<Fetched<J::Event>> {
        let name = span.name(self.key.fmt, author);
        let data = match self.cloud.get(Dir::Events, &name, MAX_EVENTS_FILE) {
            Ok(Some(d)) => d,
            Ok(None) => return Ok(Fetched::Vanished),
            Err(Error::Cloud(CloudFail::Format, m)) => return Ok(Fetched::Broken(m)),
            Err(e) => return Err(e),
        };
        match open_events::<J>(self.key, author, span.from, span.to, &data) {
            Ok(events) => Ok(Fetched::Events(events)),
            Err(e @ Error::Cloud(CloudFail::Newer, _)) => Err(e),
            Err(e) => Ok(Fetched::Broken(e.to_string())),
        }
    }

    /// Скачивает файл событий; не расшифровался — ещё одна попытка (вдруг файл испортился по дороге).
    fn fetch(&self, author: &str, span: Span) -> Result<Fetched<J::Event>> {
        match self.fetch_once(author, span)? {
            Fetched::Broken(_) => self.fetch_once(author, span),
            other => Ok(other),
        }
    }

    /// События, которые уже есть в базе, должны совпасть с облаком.
    fn check_overlap(&self, events: &[J::Event], have: u64) -> Result<()> {
        let st = lock(self.store);
        for ev in events.iter().filter(|e| J::origin(e).1 <= have) {
            if st.differs(ev)? {
                return Err(conflict());
            }
        }
        Ok(())
    }

    /// Файл не расшифровался. Если облако подключили заново, дальше обмениваться нельзя.
    /// Если ключ тот же — файл испорчен или подменён, прочитать его не сможет никто: удаляем,
    /// и устройство, у которого есть эти записи, увидит пропуск и отправит их снова.
    fn broken(&self, dir: Dir, name: &str, why: String, report: &mut Report, checked_cloud: &mut bool) -> Result<()> {
        if !*checked_cloud {
            self.same_cloud()?;
            *checked_cloud = true;
        }
        self.cloud.delete(dir, name)?;
        let source = if dir == Dir::Blobs { format!("где есть {}", self.key.fmt.blobs.this) } else { "где есть эти записи".to_string() };
        report.problems.push(format!("{why}: файл удалён, его заново отправит устройство, {source}"));
        Ok(())
    }

    fn download(&self, me: &str, files: &BTreeMap<String, Vec<Span>>, bad: &mut BTreeSet<String>, state: &mut State, report: &mut Report) -> Result<()> {
        let mut vv = known(self.store)?;
        let mut checked_cloud = false;
        for (author, spans) in files {
            loop {
                self.check()?;
                let have = vv.get(author).copied().unwrap_or(0);
                // Файл, который продолжает журнал без пропуска и уходит дальше всех.
                let next = spans.iter().filter(|s| s.from <= have + 1 && s.to > have && !bad.contains(&s.name(self.key.fmt, author))).max_by_key(|s| s.to).copied();
                let Some(span) = next else { break };
                let name = span.name(self.key.fmt, author);
                match self.fetch(author, span)? {
                    Fetched::Events(events) => {
                        self.check_overlap(&events, have)?;
                        report.received += lock(self.store).apply_remote(&events)?;
                        vv.insert(author.clone(), span.to);
                        if author == me {
                            state.checked.insert(name);
                        }
                    }
                    Fetched::Vanished => {
                        bad.insert(name);
                    }
                    Fetched::Broken(why) => {
                        self.broken(Dir::Events, &name, why, report, &mut checked_cloud)?;
                        bad.insert(name);
                    }
                }
            }
        }
        // Свои файлы, которые ещё не сверяли: база этого устройства могла начаться заново под тем же номером.
        if let Some(spans) = files.get(me) {
            let have = vv.get(me).copied().unwrap_or(0);
            for span in spans {
                let name = span.name(self.key.fmt, me);
                if span.to > have || state.checked.contains(&name) || bad.contains(&name) {
                    continue;
                }
                self.check()?;
                match self.fetch(me, *span)? {
                    Fetched::Events(events) => {
                        self.check_overlap(&events, have)?;
                        state.checked.insert(name);
                    }
                    Fetched::Vanished => {
                        bad.insert(name);
                    }
                    Fetched::Broken(why) => {
                        self.broken(Dir::Events, &name, why, report, &mut checked_cloud)?;
                        bad.insert(name);
                    }
                }
            }
        }
        Ok(())
    }

    fn upload(&self, me: &str, files: &mut BTreeMap<String, Vec<Span>>, bad: &BTreeSet<String>, state: &mut State, report: &mut Report) -> Result<()> {
        let vv = known(self.store)?;
        for (author, &max) in &vv {
            let spans = files.entry(author.clone()).or_default();
            let good: Vec<Span> = spans.iter().copied().filter(|s| !bad.contains(&s.name(self.key.fmt, author))).collect();
            for (a, b) in gaps(&good, max) {
                for span in chunks(a, b) {
                    self.check()?;
                    let events = self.events_range(author, span)?;
                    if events.len() as u64 != span.to - span.from + 1 {
                        report.problems.push(format!("в журнале устройства {author} пропуск — события {}…{} не отправлены", span.from, span.to));
                        break;
                    }
                    let (name, data) = seal_events(self.key, author, span.from, span.to, &events)?;
                    self.cloud.put(Dir::Events, &name, &data)?;
                    report.sent += events.len();
                    if !spans.contains(&span) {
                        spans.push(span);
                    }
                    if author == me {
                        state.checked.insert(name);
                    }
                }
            }
        }
        Ok(())
    }

    /// Сливает мелкие файлы устройства, если их накопилось много. Сливается только то, что есть в базе.
    fn compact(&self, me: &str, files: &mut BTreeMap<String, Vec<Span>>, bad: &BTreeSet<String>, state: &mut State) -> Result<()> {
        let vv = known(self.store)?;
        for (author, spans) in files.iter_mut() {
            let mut good: Vec<Span> = spans.iter().copied().filter(|s| !bad.contains(&s.name(self.key.fmt, author))).collect();
            if good.len() <= COMPACT_AFTER {
                continue;
            }
            let have = vv.get(author).copied().unwrap_or(0);
            good.retain(|s| s.to <= have);
            good.sort_by_key(|s| (s.from, std::cmp::Reverse(s.to)));
            let mut runs: Vec<Vec<Span>> = Vec::new();
            for s in good {
                match runs.last_mut() {
                    Some(run) if s.from <= run.iter().map(|x| x.to).max().unwrap_or(0) + 1 => run.push(s),
                    _ => runs.push(vec![s]),
                }
            }
            'run: for run in runs {
                let end = run.iter().map(|s| s.to).max().unwrap_or(0);
                let targets = chunks(run[0].from, end);
                if targets.len() >= run.len() {
                    continue;
                }
                for t in &targets {
                    if run.contains(t) {
                        continue;
                    }
                    self.check()?;
                    let events = self.events_range(author, *t)?;
                    if events.len() as u64 != t.to - t.from + 1 {
                        continue 'run;
                    }
                    let (name, data) = seal_events(self.key, author, t.from, t.to, &events)?;
                    self.cloud.put(Dir::Events, &name, &data)?;
                    if author == me {
                        state.checked.insert(name);
                    }
                }
                for s in &run {
                    if !targets.contains(s) {
                        self.check()?;
                        self.cloud.delete(Dir::Events, &s.name(self.key.fmt, author))?;
                    }
                }
                spans.retain(|s| !run.contains(s) || targets.contains(s));
                for t in targets {
                    if !spans.contains(&t) {
                        spans.push(t);
                    }
                }
            }
        }
        Ok(())
    }

    fn blobs(&self, state: &mut State, report: &mut Report, progress: &mut dyn FnMut(Step)) -> Result<()> {
        let (present, missing) = {
            let st = lock(self.store);
            (st.present_blobs()?, st.missing_blobs()?)
        };
        let named = |ids: Vec<String>| -> Vec<(String, String)> { ids.into_iter().map(|id| (self.key.blob_name(&id), id)).collect() };
        let (present, missing) = (named(present), named(missing));
        if present.is_empty() && missing.is_empty() {
            return Ok(());
        }
        let unknown = present.iter().chain(missing.iter()).any(|(n, _)| !state.blobs.contains(n));
        if unknown || self.now - state.blobs_listed >= BLOBS_RELIST {
            self.check()?;
            state.blobs = self.cloud.list(Dir::Blobs)?.into_iter().map(|e| e.name).collect();
            state.blobs_listed = self.now;
        }
        let up: Vec<&(String, String)> = present.iter().filter(|(n, _)| !state.blobs.contains(n)).collect();
        for (i, (name, id)) in up.iter().enumerate() {
            self.check()?;
            progress(Step::Upload { done: i, total: up.len() });
            let Some((mime, data)) = lock(self.store).blob(id)? else { continue };
            let (_, sealed) = seal_blob(self.key, id, &mime, &data)?;
            self.cloud.put(Dir::Blobs, name, &sealed)?;
            state.blobs.insert(name.clone());
            report.files_sent += 1;
        }
        let down: Vec<&(String, String)> = missing.iter().filter(|(n, _)| state.blobs.contains(n)).collect();
        report.files_waiting = missing.len() - down.len();
        let mut checked_cloud = false;
        for (i, (name, id)) in down.iter().enumerate() {
            self.check()?;
            progress(Step::Download { done: i, total: down.len() });
            let mut opened = None;
            for _ in 0..2 {
                opened = match self.cloud.get(Dir::Blobs, name, self.key.fmt.blobs.max_file) {
                    Ok(None) => None,
                    Ok(Some(data)) => Some(open_blob(self.key, id, name, &data)),
                    Err(e @ Error::Cloud(CloudFail::Format, _)) => Some(Err(e)),
                    Err(e) => return Err(e),
                };
                if !matches!(opened, Some(Err(_))) {
                    break;
                }
            }
            match opened {
                None => {
                    state.blobs.remove(name);
                    report.files_waiting += 1;
                }
                Some(Ok((mime, body))) => {
                    lock(self.store).put_blob_checked(id, &mime, &body)?;
                    report.files_received += 1;
                }
                Some(Err(e @ Error::Cloud(CloudFail::Newer, _))) => return Err(e),
                Some(Err(e)) => {
                    self.broken(Dir::Blobs, name, e.to_string(), report, &mut checked_cloud)?;
                    state.blobs.remove(name);
                    report.files_waiting += 1;
                }
            }
        }
        Ok(())
    }

    /// Большие файлы: отправить те, которых в облаке нет целиком, и забрать нужные — когда в облаке
    /// лежат все их куски. Недокачанный файл докачивается с того же куска в следующий раз.
    fn files(&self, report: &mut Report, progress: &mut dyn FnMut(Step)) -> Result<()> {
        let Some(cfg) = self.key.fmt.files else { return Ok(()) };
        let part = cfg.part as u64;
        let (present, wanted) = {
            let st = lock(self.store);
            if !st.files_on() {
                return Ok(());
            }
            (st.files_present()?, st.files_wanted()?)
        };
        if present.is_empty() && wanted.is_empty() {
            return Ok(());
        }
        self.check()?;
        let names: BTreeSet<String> = self.cloud.list(Dir::Files)?.into_iter().map(|e| e.name).collect();
        let parts_of = |f: &J::File| -> Vec<String> {
            let n = f.size().div_ceil(part).max(1);
            (0..n).map(|i| self.key.file_name(f.id(), i, n)).collect()
        };
        let up: Vec<(&J::File, Vec<String>)> = present.iter().map(|f| (f, parts_of(f))).filter(|(_, p)| p.iter().any(|n| !names.contains(n))).collect();
        let down: Vec<(&J::File, Vec<String>)> = wanted.iter().map(|f| (f, parts_of(f))).filter(|(_, p)| p.iter().all(|n| names.contains(n))).collect();
        report.whole_waiting = wanted.len() - down.len();
        let total = up.len() + down.len();
        for (i, (f, parts)) in up.iter().enumerate() {
            progress(Step::Files { done: i, total });
            let mut whole = true;
            for (k, name) in parts.iter().enumerate() {
                if names.contains(name) {
                    continue;
                }
                self.check()?;
                let Some((_, data)) = lock(self.store).file_read(f.id(), k as u64 * part)? else {
                    whole = false;
                    break;
                };
                let sealed = seal_file(self.key, KIND_FILE, name, &data)?;
                self.cloud.put(Dir::Files, name, &sealed)?;
            }
            if whole {
                report.whole_sent += 1;
            }
        }
        let max = self.key.fmt.max_file_part();
        for (i, (f, parts)) in down.iter().enumerate() {
            progress(Step::Files { done: up.len() + i, total });
            let have = lock(self.store).file_partial(f.id());
            let start = if have < f.size() && have.is_multiple_of(part) { have / part } else { 0 };
            for (k, name) in parts.iter().enumerate().skip(start as usize) {
                self.check()?;
                // Кусок пропал между списком и скачиванием — в другой раз.
                let Some(data) = self.cloud.get(Dir::Files, name, max)? else { break };
                let plain = match open_file(self.key, KIND_FILE, name, &data) {
                    Ok(p) => p,
                    Err(e @ Error::Cloud(CloudFail::Newer, _)) => return Err(e),
                    Err(e) => {
                        report.problems.push(format!("{} «{}»: {e}", cfg.what, f.name()));
                        break;
                    }
                };
                match lock(self.store).file_write(f, k as u64 * part, f.size(), &plain) {
                    Ok(true) => {
                        report.whole_received += 1;
                        break;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        report.problems.push(format!("{} «{}»: {e}", cfg.what, f.name()));
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Обмен с облаком: забрать новое, отправить недостающее, слить мелкие файлы, вложения, большие файлы.
/// `state` меняется по ходу: сохраните его и при ошибке — сделанное не повторится.
pub fn sync<J: CloudStore>(r: &Round<J>, state: &mut State, progress: &mut dyn FnMut(Step)) -> Result<Report> {
    let mut report = Report::default();
    progress(Step::Events);
    r.check()?;
    if !r.cloud.exists(Dir::Root, r.key.fmt.key_file)? {
        return Err(r.key.fmt.gone());
    }
    let me = lock(r.store).device();
    let mut files = r.list_events()?;
    if files.is_empty() && state.synced {
        // Файлы пропали: облако очистили или подключили заново. Если ключ тот же — отправим всё снова.
        r.same_cloud()?;
    }
    let mut bad = BTreeSet::new();
    r.download(&me, &files, &mut bad, state, &mut report)?;
    r.upload(&me, &mut files, &bad, state, &mut report)?;
    r.compact(&me, &mut files, &bad, state)?;
    let fmt = r.key.fmt;
    let names: BTreeSet<String> = files.iter().flat_map(|(a, spans)| spans.iter().map(move |s| s.name(fmt, a))).collect();
    state.checked.retain(|n| names.contains(n));
    r.blobs(state, &mut report, progress)?;
    r.files(&mut report, progress)?;
    state.synced = true;
    Ok(report)
}

// ——— Облако в памяти: для проверок ———

/// Облако в памяти — для проверок обмена без сети.
#[derive(Default)]
pub struct MemCloud {
    files: Mutex<BTreeMap<(Dir, String), Vec<u8>>>,
    /// Сколько было обращений.
    pub calls: AtomicUsize,
}

impl MemCloud {
    fn files(&self) -> MutexGuard<'_, BTreeMap<(Dir, String), Vec<u8>>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn names(&self, dir: Dir) -> Vec<String> {
        self.files().keys().filter(|(d, _)| *d == dir).map(|(_, n)| n.clone()).collect()
    }

    pub fn raw(&self, dir: Dir, name: &str) -> Option<Vec<u8>> {
        self.files().get(&(dir, name.to_string())).cloned()
    }

    pub fn set_raw(&self, dir: Dir, name: &str, data: Vec<u8>) {
        self.files().insert((dir, name.to_string()), data);
    }

    pub fn remove(&self, dir: Dir, name: &str) {
        self.files().remove(&(dir, name.to_string()));
    }

    fn call(&self) {
        self.calls.fetch_add(1, Ordering::Relaxed);
    }
}

impl Cloud for MemCloud {
    fn list(&self, dir: Dir) -> Result<Vec<Entry>> {
        self.call();
        Ok(self.files().iter().filter(|((d, _), _)| *d == dir).map(|((_, n), v)| Entry { name: n.clone(), size: v.len() as u64 }).collect())
    }

    fn get(&self, dir: Dir, name: &str, max: usize) -> Result<Option<Vec<u8>>> {
        self.call();
        match self.files().get(&(dir, name.to_string())) {
            Some(v) if v.len() > max => Err(fail(CloudFail::Format, format!("файл в облаке больше {max} байт ({name})"))),
            other => Ok(other.cloned()),
        }
    }

    fn put(&self, dir: Dir, name: &str, data: &[u8]) -> Result<()> {
        self.call();
        self.files().insert((dir, name.to_string()), data.to_vec());
        Ok(())
    }

    fn exists(&self, dir: Dir, name: &str) -> Result<bool> {
        self.call();
        Ok(self.files().contains_key(&(dir, name.to_string())))
    }

    fn delete(&self, dir: Dir, name: &str) -> Result<()> {
        self.call();
        self.files().remove(&(dir, name.to_string()));
        Ok(())
    }
}

#[cfg(test)]
mod tests;
