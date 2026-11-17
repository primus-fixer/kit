//! Резервная копия: журнал событий и вложения приложения в одном файле, зашифрованном паролем копии.
//!
//! Ключ базы хранится в защищённом хранилище устройства и с копией не переносится, поэтому у копии свой
//! ключ — Argon2id от пароля копии. Им зашифрован случайный ключ файла, а содержимое идёт кусками по 1 МБ
//! (STREAM на XChaCha20-Poly1305): порча, подмена, перестановка и обрезка любого куска видны при чтении.
//!
//! Файл: заголовок (130 байт: метка приложения, формат, параметры ключа, соль, зашифрованный ключ файла)
//! и куски. Внутри кусков — записи: сведения о копии, пачки событий, вложения, записи приложения и итог.
//! Записи незнакомого вида (из более новой версии) при чтении пропускаются.
//!
//! Формат — тот, в котором «Карман» и «Полка» делали копии до общего модуля; чем приложения различаются
//! (метка, расширение, название, свои записи), описывает [`Format`]. Что класть в копию и как добавлять
//! прочитанное в базу, решает приложение.

use crate::error::{Error, Result};
use crate::AppNames;
use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chacha20poly1305::aead::stream::{DecryptorBE32, EncryptorBE32};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

/// Версия формата файла.
pub const FORMAT: u16 = 1;
/// Самый короткий пароль копии.
pub const MIN_PASSWORD: usize = 8;

/// Кусок открытого текста.
const CHUNK: usize = 1 << 20;
/// Начало заголовка: метка, формат, вид ключа, параметры, соль.
const PREFIX: usize = 8 + 2 + 1 + 12 + 16;
/// Заголовок целиком: ещё nonce и зашифрованный ключ файла, nonce кусков.
const HEADER: usize = PREFIX + 24 + 48 + 19;
/// Старший бит длины куска — «последний кусок».
const LAST: u32 = 1 << 31;
const TAG: usize = 16;
/// Предел одной записи (вложение — до 15 МБ, пачка событий — меньше).
const MAX_RECORD: usize = 32 << 20;
/// Событий в одной записи.
const BATCH: usize = 500;

const REC_MANIFEST: u8 = 1;
const REC_EVENTS: u8 = 2;
const REC_BLOB: u8 = 3;
const REC_END: u8 = 0xFF;

/// Чем копии приложения отличаются от копий других приложений.
#[derive(Clone, Copy, Debug)]
pub struct Format {
    /// Метка в начале файла: `KARMANBK`. **У выпущенного приложения не менять** — старые копии перестанут
    /// узнаваться.
    pub magic: &'static [u8; 8],
    /// Расширение файла копии: `karman`.
    pub ext: &'static str,
    /// Название для сообщений: «Это не резервная копия „Кармана“».
    pub names: AppNames,
    /// Слова-пароли этого приложения, которые перебирают первыми: «karman», «карман».
    pub common: &'static [&'static str],
    /// Свои виды записей приложения и имя счётчика в итоге: `(4, "books")`. Номера 1–3 и 255 заняты.
    pub records: &'static [(u8, &'static str)],
}

/// 32 (или N) случайных байта.
pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| Error::Random(e.to_string()))?;
    Ok(b)
}

/// Затирает ключ в памяти.
pub fn wipe(b: &mut [u8]) {
    b.fill(0);
    std::hint::black_box(b);
}

fn broken(what: &str) -> Error {
    Error::Backup(format!("Копия повреждена: {what}"))
}

/// Параметры Argon2id: память (КиБ), проходы, потоки.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kdf {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Kdf {
    /// 64 МБ и 3 прохода: на телефоне около секунды, перебор паролей — дорогой.
    pub const DEFAULT: Kdf = Kdf { m_kib: 64 * 1024, t: 3, p: 1 };

    /// Чужой или испорченный файл не должен заставить считать ключ часами или съесть всю память.
    pub fn valid(self) -> bool {
        (8 * 1024..=256 * 1024).contains(&self.m_kib) && (1..=10).contains(&self.t) && (1..=4).contains(&self.p)
    }

    fn check(self) -> Result<()> {
        if self.valid() {
            Ok(())
        } else {
            Err(Error::Backup("Копия не открывается: неподходящие параметры ключа".into()))
        }
    }
}

/// Argon2id от пароля — общий для копий и облака. Параметры должны быть проверены ([`Kdf::valid`]).
pub fn derive_raw(password: &str, salt: &[u8; 16], kdf: Kdf) -> Result<[u8; 32]> {
    let params = Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32)).map_err(|e| Error::Backup(e.to_string()))?;
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| Error::Backup(e.to_string()))?;
    Ok(key)
}

/// Пароль копии: не короче восьми символов.
pub fn check_password(password: &str) -> Result<()> {
    if password.chars().count() < MIN_PASSWORD {
        return Err(Error::Backup(format!("Пароль копии — не короче {MIN_PASSWORD} символов")));
    }
    Ok(())
}

/// Частые пароли и слова-пароли (буквы без цифр и знаков): их перебирают первыми.
const COMMON: &[&str] = &[
    "password", "passw", "pass", "parol", "gfhjkm", "пароль", "qwerty", "qwertyuiop", "йцукен", "admin", "letmein", "iloveyou", "welcome", "1q2w3e4r", "1q2w3e4r5t", "zaq12wsx", "1qaz2wsx", "привет", "privet",
    "любовь", "sunshine", "dragon", "monkey",
];
/// Ряды клавиш и символов подряд: «12345678», «qwertyui», «йцукенгш», «abcdefgh».
const RUNS: &[&str] = &[
    "01234567890123456789",
    "abcdefghijklmnopqrstuvwxyz",
    "qwertyuiopasdfghjklzxcvbnm",
    "1qaz2wsx3edc4rfv5tgb6yhn",
    "йцукенгшщзхъфывапролджэячсмитьбю",
    "абвгдеёжзийклмнопрстуфхцчшщъыьэюя",
];

/// Самые простые пароли: подбираются за минуты, даже с Argon2id. Возвращает, чем плох пароль.
pub fn weak_password(fmt: &Format, password: &str) -> Option<&'static str> {
    let s: String = password.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    let chars: Vec<char> = s.chars().collect();
    let distinct = chars.iter().collect::<std::collections::BTreeSet<_>>().len();
    if distinct < 4 {
        return Some("в нём почти одни и те же символы");
    }
    if chars.iter().all(|c| c.is_ascii_digit()) && chars.len() < 12 {
        return Some("одни цифры подбираются быстро");
    }
    for k in 1..=4 {
        if chars.len() > k && chars.len().is_multiple_of(k) && chars.chunks(k).all(|c| c == &chars[..k]) {
            return Some("это повтор нескольких символов");
        }
    }
    let rev: String = s.chars().rev().collect();
    if RUNS.iter().any(|r| r.contains(s.as_str()) || r.contains(rev.as_str())) {
        return Some("это ряд клавиш или символов подряд");
    }
    let letters: String = s.chars().filter(|c| !c.is_ascii_digit() && !c.is_ascii_punctuation()).collect();
    if COMMON.iter().chain(fmt.common).any(|w| s == *w || letters == *w) {
        return Some("это одно из самых частых слов-паролей");
    }
    None
}

/// Новый пароль копий: не короче восьми символов и не из самых простых — им шифруются копии и данные
/// в облаке, а файлы на Диске может скачать любой, кто войдёт в аккаунт.
pub fn check_new_password(fmt: &Format, password: &str) -> Result<()> {
    check_password(password)?;
    match weak_password(fmt, password) {
        Some(why) => Err(Error::Backup(format!("Слишком простой пароль: {why}. Им шифруются копии и данные в облаке — лучше фраза из нескольких слов"))),
        None => Ok(()),
    }
}

/// Ключ копий: Argon2id от пароля копии. Приложение хранит его в базе (она сама зашифрована), чтобы
/// автокопии делались без пароля; сам пароль нигде не хранится.
pub struct BackupKey {
    salt: [u8; 16],
    kdf: Kdf,
    key: [u8; 32],
}

impl Drop for BackupKey {
    fn drop(&mut self) {
        wipe(&mut self.key);
    }
}

impl BackupKey {
    /// Новый ключ из пароля, со случайной солью.
    pub fn new(password: &str) -> Result<BackupKey> {
        BackupKey::new_with(password, Kdf::DEFAULT)
    }

    /// То же с другими параметрами (в проверках — побыстрее).
    pub fn new_with(password: &str, kdf: Kdf) -> Result<BackupKey> {
        check_password(password)?;
        BackupKey::derive(password, random()?, kdf)
    }

    pub fn derive(password: &str, salt: [u8; 16], kdf: Kdf) -> Result<BackupKey> {
        kdf.check()?;
        Ok(BackupKey { salt, kdf, key: derive_raw(password, &salt, kdf)? })
    }

    /// Этот ли пароль у ключа копий: тот же пароль с той же солью даёт тот же ключ.
    pub fn matches(&self, password: &str) -> Result<bool> {
        let mut k = derive_raw(password, &self.salt, self.kdf)?;
        let same = k.iter().zip(self.key.iter()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
        wipe(&mut k);
        Ok(same)
    }

    /// Для хранения в базе: «1:память:проходы:потоки:соль:ключ».
    pub fn to_meta(&self) -> String {
        format!("1:{}:{}:{}:{}:{}", self.kdf.m_kib, self.kdf.t, self.kdf.p, B64.encode(self.salt), B64.encode(self.key))
    }

    pub fn from_meta(s: &str) -> Result<BackupKey> {
        let bad = || Error::Backup("Ключ копий повреждён: задайте пароль копии заново".into());
        let p: Vec<&str> = s.split(':').collect();
        if p.len() != 6 || p[0] != "1" {
            return Err(bad());
        }
        let num = |x: &str| x.parse::<u32>().map_err(|_| bad());
        let kdf = Kdf { m_kib: num(p[1])?, t: num(p[2])?, p: num(p[3])? };
        let salt = <[u8; 16]>::try_from(B64.decode(p[4]).map_err(|_| bad())?.as_slice()).map_err(|_| bad())?;
        let mut raw = B64.decode(p[5]).map_err(|_| bad())?;
        let key = <[u8; 32]>::try_from(raw.as_slice()).map_err(|_| bad());
        wipe(&mut raw);
        Ok(BackupKey { salt, kdf, key: key? })
    }
}

/// Сведения о копии — первая запись. По ним видно, что восстанавливается. `C` — счётчики приложения
/// (у «Кармана» — операции и поездки, у «Полки» — записи и файлы книг): в файле они лежат рядом с общими.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest<C> {
    pub format: u16,
    /// Версия приложения, которое сделало копию.
    pub app: String,
    /// Когда сделана: секунды Unix.
    pub created: i64,
    /// Устройство: имя и номер.
    pub device: String,
    pub device_id: String,
    pub events: u64,
    pub blobs: u64,
    #[serde(flatten)]
    pub counts: C,
    /// Поля из более новых версий.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Кто и когда делает копию.
pub struct MadeBy {
    pub app: String,
    pub created: i64,
    pub device: String,
    pub device_id: String,
}

impl<C> Manifest<C> {
    pub fn new(by: &MadeBy, events: u64, blobs: u64, counts: C) -> Manifest<C> {
        Manifest { format: FORMAT, app: by.app.clone(), created: by.created, device: by.device.clone(), device_id: by.device_id.clone(), events, blobs, counts, extra: Default::default() }
    }
}

/// Последняя запись: сколько событий, вложений и записей приложения в копии на самом деле.
#[derive(Serialize, Deserialize)]
struct End {
    events: u64,
    blobs: u64,
    /// Записи приложения по именам из [`Format::records`]: у «Полки» — `books`.
    #[serde(flatten)]
    records: BTreeMap<String, u64>,
}

/// Что записано.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Made {
    pub events: u64,
    pub blobs: u64,
    /// Записей приложения по именам из [`Format::records`].
    pub records: BTreeMap<String, u64>,
    pub bytes: u64,
}

fn header_prefix(fmt: &Format, kdf: Kdf, salt: &[u8; 16]) -> [u8; PREFIX] {
    let mut h = [0u8; PREFIX];
    h[..8].copy_from_slice(fmt.magic);
    h[8..10].copy_from_slice(&FORMAT.to_le_bytes());
    h[10] = 1; // Argon2id
    h[11..15].copy_from_slice(&kdf.m_kib.to_le_bytes());
    h[15..19].copy_from_slice(&kdf.t.to_le_bytes());
    h[19..23].copy_from_slice(&kdf.p.to_le_bytes());
    h[23..39].copy_from_slice(salt);
    h
}

fn record_name(fmt: &Format, kind: u8) -> Option<&'static str> {
    fmt.records.iter().find(|(k, _)| *k == kind).map(|(_, n)| *n)
}

/// Файл копии пишется по частям: события, затем вложения по одному — в памяти не больше куска.
pub struct Writer<W: Write> {
    fmt: Format,
    out: W,
    enc: Option<EncryptorBE32<XChaCha20Poly1305>>,
    header: [u8; HEADER],
    buf: Vec<u8>,
    bytes: u64,
    events: u64,
    blobs: u64,
    records: BTreeMap<String, u64>,
}

impl<W: Write> Writer<W> {
    pub fn new<C: Serialize>(fmt: &Format, mut out: W, key: &BackupKey, manifest: &Manifest<C>) -> Result<Writer<W>> {
        let mut dek: [u8; 32] = random()?;
        let wrap_nonce: [u8; 24] = random()?;
        let stream_nonce: [u8; 19] = random()?;
        let prefix = header_prefix(fmt, key.kdf, &key.salt);
        let wrapped = XChaCha20Poly1305::new((&key.key).into())
            .encrypt(XNonce::from_slice(&wrap_nonce), Payload { msg: &dek, aad: &prefix })
            .map_err(|_| Error::Backup("не удалось зашифровать ключ файла".into()))?;
        let mut header = [0u8; HEADER];
        header[..PREFIX].copy_from_slice(&prefix);
        header[PREFIX..PREFIX + 24].copy_from_slice(&wrap_nonce);
        header[PREFIX + 24..PREFIX + 72].copy_from_slice(&wrapped);
        header[PREFIX + 72..].copy_from_slice(&stream_nonce);
        out.write_all(&header)?;
        let enc = EncryptorBE32::from_aead(XChaCha20Poly1305::new((&dek).into()), stream_nonce.as_ref().into());
        wipe(&mut dek);
        let records = fmt.records.iter().map(|(_, n)| (n.to_string(), 0)).collect();
        let mut w = Writer { fmt: *fmt, out, enc: Some(enc), header, buf: Vec::with_capacity(CHUNK + 64), bytes: HEADER as u64, events: 0, blobs: 0, records };
        w.record(REC_MANIFEST, &serde_json::to_vec(manifest)?)?;
        Ok(w)
    }

    fn frame(&mut self, ct: &[u8], last: bool) -> Result<()> {
        let len = ct.len() as u32 | if last { LAST } else { 0 };
        self.out.write_all(&len.to_le_bytes())?;
        self.out.write_all(ct)?;
        self.bytes += 4 + ct.len() as u64;
        Ok(())
    }

    fn put(&mut self, data: &[u8]) -> Result<()> {
        self.buf.extend_from_slice(data);
        while self.buf.len() >= CHUNK {
            let enc = self.enc.as_mut().ok_or_else(|| Error::Backup("копия уже закрыта".into()))?;
            let ct = enc.encrypt_next(Payload { msg: &self.buf[..CHUNK], aad: &self.header }).map_err(|_| Error::Backup("не удалось зашифровать копию".into()))?;
            self.buf.drain(..CHUNK);
            self.frame(&ct, false)?;
        }
        Ok(())
    }

    fn record(&mut self, kind: u8, payload: &[u8]) -> Result<()> {
        if payload.len() > MAX_RECORD {
            return Err(Error::Backup("слишком большая запись для копии".into()));
        }
        self.put(&[kind])?;
        self.put(&(payload.len() as u32).to_le_bytes())?;
        self.put(payload)
    }

    /// События журнала — пачками по 500.
    pub fn events<E: Serialize>(&mut self, events: &[E]) -> Result<()> {
        for batch in events.chunks(BATCH) {
            self.record(REC_EVENTS, &serde_json::to_vec(batch)?)?;
            self.events += batch.len() as u64;
        }
        Ok(())
    }

    /// Вложение: снимок чека, обложка.
    pub fn blob(&mut self, id: &str, mime: &str, data: &[u8]) -> Result<()> {
        if id.len() > 255 || mime.len() > 255 {
            return Err(Error::Backup("неверное вложение".into()));
        }
        let mut p = Vec::with_capacity(2 + id.len() + mime.len() + data.len());
        p.push(id.len() as u8);
        p.extend_from_slice(id.as_bytes());
        p.push(mime.len() as u8);
        p.extend_from_slice(mime.as_bytes());
        p.extend_from_slice(data);
        self.record(REC_BLOB, &p)?;
        self.blobs += 1;
        Ok(())
    }

    /// Запись приложения: вид — из [`Format::records`], содержимое — как приложению удобно.
    pub fn custom(&mut self, kind: u8, payload: &[u8]) -> Result<()> {
        let name = record_name(&self.fmt, kind).ok_or_else(|| Error::Backup(format!("неизвестный вид записи {kind}")))?;
        self.record(kind, payload)?;
        *self.records.entry(name.to_string()).or_default() += 1;
        Ok(())
    }

    /// Итог и последний кусок. Возвращает, сколько записано, и куда писали.
    pub fn finish(mut self) -> Result<(Made, W)> {
        let end = serde_json::to_vec(&End { events: self.events, blobs: self.blobs, records: self.records.clone() })?;
        self.record(REC_END, &end)?;
        let enc = self.enc.take().ok_or_else(|| Error::Backup("копия уже закрыта".into()))?;
        let ct = enc.encrypt_last(Payload { msg: &self.buf, aad: &self.header }).map_err(|_| Error::Backup("не удалось зашифровать копию".into()))?;
        self.buf.clear();
        self.frame(&ct, true)?;
        self.out.flush()?;
        Ok((Made { events: self.events, blobs: self.blobs, records: self.records, bytes: self.bytes }, self.out))
    }
}

/// Что встретилось в копии.
pub enum Item<E> {
    Events(Vec<E>),
    Blob { id: String, mime: String, data: Vec<u8> },
    /// Запись приложения из [`Format::records`].
    Custom { kind: u8, payload: Vec<u8> },
}

/// Чтение копии: ключ проверяется сразу, куски — по мере чтения.
pub struct Reader<R: Read, C> {
    fmt: Format,
    input: R,
    dec: Option<DecryptorBE32<XChaCha20Poly1305>>,
    header: [u8; HEADER],
    plain: Vec<u8>,
    pos: usize,
    /// Последний кусок расшифрован.
    last: bool,
    /// Итоговая запись прочитана и сошлась.
    ended: bool,
    /// Сколько событий и вложений прочитано.
    pub events: u64,
    pub blobs: u64,
    /// Записей приложения по именам из [`Format::records`].
    pub records: BTreeMap<String, u64>,
    pub manifest: Manifest<C>,
}

/// Файл начинается как копия этого приложения.
pub fn is_backup(fmt: &Format, path: &Path) -> bool {
    let mut head = [0u8; 8];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == fmt.magic
}

fn read_header<R: Read>(fmt: &Format, input: &mut R) -> Result<([u8; HEADER], Kdf, [u8; 16])> {
    let mut header = [0u8; HEADER];
    let mut got = 0;
    while got < HEADER {
        let n = input.read(&mut header[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    if got < 8 || &header[..8] != fmt.magic {
        return Err(Error::Backup(format!("Это не резервная копия «{}»", fmt.names.name_gen)));
    }
    if got < HEADER {
        return Err(broken("файл обрывается"));
    }
    let format = u16::from_le_bytes([header[8], header[9]]);
    if format > FORMAT {
        return Err(Error::Backup(format!("Копия сделана в более новой версии «{}» — обновите приложение", fmt.names.name_gen)));
    }
    if format == 0 || header[10] != 1 {
        return Err(broken("неизвестный формат"));
    }
    let num = |i: usize| u32::from_le_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]);
    let kdf = Kdf { m_kib: num(11), t: num(15), p: num(19) };
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&header[23..39]);
    Ok((header, kdf, salt))
}

impl<R: Read, C: DeserializeOwned> Reader<R, C> {
    /// Открывает копию паролем (ключ считается заново — около секунды).
    pub fn open(fmt: &Format, mut input: R, password: &str) -> Result<Reader<R, C>> {
        let (header, kdf, salt) = read_header(fmt, &mut input)?;
        kdf.check()?;
        let key = BackupKey::derive(password, salt, kdf)?;
        Reader::start(fmt, input, header, &key)
    }

    /// Открывает копию сохранённым ключом — проверка только что записанной копии.
    pub fn open_with_key(fmt: &Format, mut input: R, key: &BackupKey) -> Result<Reader<R, C>> {
        let (header, kdf, salt) = read_header(fmt, &mut input)?;
        if kdf != key.kdf || salt != key.salt {
            return Err(Error::BackupPassword);
        }
        Reader::start(fmt, input, header, key)
    }

    fn start(fmt: &Format, input: R, header: [u8; HEADER], key: &BackupKey) -> Result<Reader<R, C>> {
        let mut dek = XChaCha20Poly1305::new((&key.key).into())
            .decrypt(XNonce::from_slice(&header[PREFIX..PREFIX + 24]), Payload { msg: &header[PREFIX + 24..PREFIX + 72], aad: &header[..PREFIX] })
            .map_err(|_| Error::BackupPassword)?;
        let dek_arr = <[u8; 32]>::try_from(dek.as_slice()).map_err(|_| Error::BackupPassword);
        wipe(&mut dek);
        let mut dek_arr = dek_arr?;
        let dec = DecryptorBE32::from_aead(XChaCha20Poly1305::new((&dek_arr).into()), header[PREFIX + 72..].into());
        wipe(&mut dek_arr);
        let mut r = Reader { fmt: *fmt, input, dec: Some(dec), header, plain: Vec::new(), pos: 0, last: false, ended: false, events: 0, blobs: 0, records: BTreeMap::new(), manifest: Manifest::<Option<C>>::new(&MadeBy { app: String::new(), created: 0, device: String::new(), device_id: String::new() }, 0, 0, None) };
        let (kind, payload) = r.record()?;
        if kind != REC_MANIFEST {
            return Err(broken("нет сведений о копии"));
        }
        let manifest: Manifest<C> = serde_json::from_slice(&payload).map_err(|_| broken("сведения о копии не читаются"))?;
        Ok(Reader {
            fmt: r.fmt,
            input: r.input,
            dec: r.dec,
            header: r.header,
            plain: r.plain,
            pos: r.pos,
            last: r.last,
            ended: r.ended,
            events: 0,
            blobs: 0,
            records: fmt.records.iter().map(|(_, n)| (n.to_string(), 0)).collect(),
            manifest,
        })
    }
}

impl<R: Read, C> Reader<R, C> {
    /// Следующий кусок. `false` — копия кончилась.
    fn next_chunk(&mut self) -> Result<bool> {
        if self.last {
            return Ok(false);
        }
        let mut len = [0u8; 4];
        let mut got = 0;
        while got < 4 {
            let n = self.input.read(&mut len[got..])?;
            if n == 0 {
                return Err(broken("файл обрывается — копия не дописана или обрезана"));
            }
            got += n;
        }
        let len = u32::from_le_bytes(len);
        let last = len & LAST != 0;
        let n = (len & !LAST) as usize;
        if !(TAG..=CHUNK + TAG).contains(&n) {
            return Err(broken("неверная длина куска"));
        }
        let mut ct = vec![0u8; n];
        self.input.read_exact(&mut ct).map_err(|_| broken("файл обрывается — копия не дописана или обрезана"))?;
        let payload = Payload { msg: &ct, aad: &self.header };
        self.plain = if last {
            self.last = true;
            let dec = self.dec.take().ok_or_else(|| broken("лишний кусок"))?;
            dec.decrypt_last(payload).map_err(|_| broken("кусок изменён или переставлен"))?
        } else {
            let dec = self.dec.as_mut().ok_or_else(|| broken("лишний кусок"))?;
            dec.decrypt_next(payload).map_err(|_| broken("кусок изменён или переставлен"))?
        };
        self.pos = 0;
        if last {
            // После последнего куска в файле ничего быть не должно.
            let mut extra = [0u8; 1];
            if self.input.read(&mut extra)? != 0 {
                return Err(broken("после конца копии лишние данные"));
            }
        }
        Ok(true)
    }

    fn take(&mut self, out: &mut [u8]) -> Result<()> {
        let mut got = 0;
        while got < out.len() {
            if self.pos == self.plain.len() && !self.next_chunk()? {
                return Err(broken("копия кончается посреди записи"));
            }
            let n = (out.len() - got).min(self.plain.len() - self.pos);
            out[got..got + n].copy_from_slice(&self.plain[self.pos..self.pos + n]);
            self.pos += n;
            got += n;
        }
        Ok(())
    }

    fn record(&mut self) -> Result<(u8, Vec<u8>)> {
        let mut head = [0u8; 5];
        self.take(&mut head)?;
        let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
        if len > MAX_RECORD {
            return Err(broken("слишком большая запись"));
        }
        let mut payload = vec![0u8; len];
        self.take(&mut payload)?;
        Ok((head[0], payload))
    }

    /// Следующая запись: пачка событий, вложение или запись приложения. `None` — копия прочитана целиком
    /// и итог сошёлся.
    pub fn next_item<E: DeserializeOwned>(&mut self) -> Result<Option<Item<E>>> {
        loop {
            if self.ended {
                return Ok(None);
            }
            let (kind, payload) = self.record()?;
            match kind {
                REC_EVENTS => {
                    let events: Vec<E> = serde_json::from_slice(&payload).map_err(|_| broken("события не читаются"))?;
                    self.events += events.len() as u64;
                    return Ok(Some(Item::Events(events)));
                }
                REC_BLOB => {
                    let bad = || broken("вложение не читается");
                    let id_len = *payload.first().ok_or_else(bad)? as usize;
                    let id = std::str::from_utf8(payload.get(1..1 + id_len).ok_or_else(bad)?).map_err(|_| bad())?.to_string();
                    let mime_len = *payload.get(1 + id_len).ok_or_else(bad)? as usize;
                    let at = 2 + id_len;
                    let mime = std::str::from_utf8(payload.get(at..at + mime_len).ok_or_else(bad)?).map_err(|_| bad())?.to_string();
                    let data = payload[at + mime_len..].to_vec();
                    self.blobs += 1;
                    return Ok(Some(Item::Blob { id, mime, data }));
                }
                REC_END => {
                    let end: End = serde_json::from_slice(&payload).map_err(|_| broken("итог не читается"))?;
                    // Записей приложения, которых нет в итоге (копия старой версии), — ноль.
                    let records_match = self.records.iter().all(|(name, n)| end.records.get(name).copied().unwrap_or(0) == *n);
                    if end.events != self.events || end.blobs != self.blobs || !records_match {
                        return Err(broken("не хватает записей"));
                    }
                    // Итог — последняя запись последнего куска.
                    if self.pos != self.plain.len() || !self.last {
                        return Err(broken("после итога ещё данные"));
                    }
                    self.ended = true;
                    return Ok(None);
                }
                REC_MANIFEST => return Err(broken("сведения о копии повторяются")),
                _ => {
                    if let Some(name) = record_name(&self.fmt, kind) {
                        *self.records.entry(name.to_string()).or_default() += 1;
                        return Ok(Some(Item::Custom { kind, payload }));
                    }
                    // Записи из более новой версии того же формата пропускаются.
                    continue;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
