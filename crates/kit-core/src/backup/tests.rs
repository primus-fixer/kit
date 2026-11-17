use super::*;
use serde_json::{json, Value};
use std::io::Cursor;

const FAST: Kdf = Kdf { m_kib: 8 * 1024, t: 1, p: 1 };

/// Копии «Кармана» — как до общего модуля.
const KARMAN: Format = Format {
    magic: b"KARMANBK",
    ext: "karman",
    names: AppNames { id: "karman", name: "Карман", name_acc: "Карман", name_gen: "Кармана", feminine: false },
    common: &["karman", "карман"],
    records: &[],
};

/// Копии «Полки»: запись 4 — кусок файла книги.
const POLKA: Format = Format {
    magic: b"POLKABAK",
    ext: "polka",
    names: AppNames { id: "polka", name: "Полка", name_acc: "Полку", name_gen: "Полки", feminine: true },
    common: &["polka", "полка"],
    records: &[(4, "books")],
};

/// Счётчики «Кармана» в сведениях о копии.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct KarmanCounts {
    ops: u64,
    trips: u64,
}

/// Счётчики «Полки»: файлов книг нет в копиях до 0.8.0.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PolkaCounts {
    titles: u64,
    #[serde(default)]
    books: u64,
}

fn by() -> MadeBy {
    MadeBy { app: "0.9.0".into(), created: 1_762_540_200, device: "Ноутбук".into(), device_id: "dev-a".into() }
}

fn bytes(n: usize, fill: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u32).wrapping_mul(2_654_435_761).to_be_bytes()[0] ^ fill).collect()
}

/// Копия «Кармана»: 1200 событий (три пачки) и два вложения, одно — больше куска.
fn karman_file(key: &BackupKey) -> Vec<u8> {
    let events: Vec<Value> = (0..1200).map(|i| json!({ "device": "dev-a", "seq": i + 1, "body": { "kind": "op", "n": i } })).collect();
    let manifest = Manifest::new(&by(), events.len() as u64, 2, KarmanCounts { ops: 52, trips: 5 });
    let mut w = Writer::new(&KARMAN, Vec::new(), key, &manifest).unwrap();
    w.events(&events).unwrap();
    w.blob("small", "image/jpeg", &bytes(40_000, 1)).unwrap();
    w.blob("big", "image/jpeg", &bytes(CHUNK + 300_000, 2)).unwrap();
    let (made, file) = w.finish().unwrap();
    assert_eq!((made.events, made.blobs, made.records.len()), (1200, 2, 0));
    assert_eq!(made.bytes, file.len() as u64);
    file
}

struct Got<C> {
    manifest: Manifest<C>,
    events: Vec<Value>,
    blobs: Vec<(String, String, usize)>,
    custom: Vec<(u8, usize)>,
    records: BTreeMap<String, u64>,
}

fn read_all<C: DeserializeOwned>(fmt: &Format, data: &[u8], password: &str) -> Result<Got<C>> {
    let mut r: Reader<_, C> = Reader::open(fmt, Cursor::new(data), password)?;
    let (mut events, mut blobs, mut custom) = (Vec::new(), Vec::new(), Vec::new());
    while let Some(item) = r.next_item::<Value>()? {
        match item {
            Item::Events(e) => events.extend(e),
            Item::Blob { id, mime, data } => blobs.push((id, mime, data.len())),
            Item::Custom { kind, payload } => custom.push((kind, payload.len())),
        }
    }
    assert_eq!((r.events, r.blobs), (events.len() as u64, blobs.len() as u64));
    Ok(Got { manifest: r.manifest, events, blobs, custom, records: r.records })
}

#[test]
fn writes_and_reads_everything() {
    let key = BackupKey::new_with("отпуск в Сочи", FAST).unwrap();
    let file = karman_file(&key);
    let got: Got<KarmanCounts> = read_all(&KARMAN, &file, "отпуск в Сочи").unwrap();
    assert_eq!(got.manifest.counts, KarmanCounts { ops: 52, trips: 5 });
    assert_eq!((got.manifest.app.as_str(), got.manifest.device.as_str(), got.manifest.events), ("0.9.0", "Ноутбук", 1200));
    assert_eq!(got.events.len(), 1200);
    assert_eq!(got.events[1199]["body"]["n"], 1199);
    assert_eq!(got.blobs, vec![("small".into(), "image/jpeg".into(), 40_000), ("big".into(), "image/jpeg".into(), CHUNK + 300_000)]);
    assert!(got.custom.is_empty());
}

/// Сведения о копии и итог лежат в файле так же, как у приложений до kit: старая версия прочитает новую копию.
#[test]
fn manifest_and_end_keep_their_layout() {
    let m = Manifest::new(&by(), 108, 2, KarmanCounts { ops: 52, trips: 5 });
    assert_eq!(
        serde_json::to_string(&m).unwrap(),
        r#"{"format":1,"app":"0.9.0","created":1762540200,"device":"Ноутбук","device_id":"dev-a","events":108,"blobs":2,"ops":52,"trips":5}"#
    );
    // Поля из более новой версии сохраняются при чтении.
    let newer: Manifest<KarmanCounts> = serde_json::from_str(r#"{"format":1,"app":"9.0","created":1,"device":"d","device_id":"x","events":1,"blobs":0,"ops":1,"trips":0,"photos":3}"#).unwrap();
    assert_eq!(newer.extra["photos"], 3);
    assert_eq!(newer.counts, KarmanCounts { ops: 1, trips: 0 });
    // Копия «Полки» до 0.8.0: файлов книг в сведениях ещё нет.
    let old: Manifest<PolkaCounts> = serde_json::from_str(r#"{"format":1,"app":"0.7.0","created":1,"device":"d","device_id":"x","events":1,"blobs":0,"titles":4}"#).unwrap();
    assert_eq!(old.counts, PolkaCounts { titles: 4, books: 0 });
    assert!(old.extra.is_empty());

    assert_eq!(serde_json::to_string(&End { events: 3, blobs: 1, records: BTreeMap::new() }).unwrap(), r#"{"events":3,"blobs":1}"#);
    assert_eq!(serde_json::to_string(&End { events: 3, blobs: 1, records: [("books".to_string(), 2)].into() }).unwrap(), r#"{"events":3,"blobs":1,"books":2}"#);
}

/// Записи приложения считаются в итоге: недостающую или лишнюю видно.
#[test]
fn custom_records_are_counted() {
    let key = BackupKey::new_with("книги на даче", FAST).unwrap();
    let manifest = Manifest::new(&by(), 1, 0, PolkaCounts { titles: 1, books: 1 });
    let mut w = Writer::new(&POLKA, Vec::new(), &key, &manifest).unwrap();
    w.events(&[json!({ "kind": "title" })]).unwrap();
    w.custom(4, b"part one").unwrap();
    w.custom(4, b"part two").unwrap();
    assert!(w.custom(9, b"?").is_err(), "вид записи, которого приложение не объявило");
    let (made, file) = w.finish().unwrap();
    assert_eq!(made.records["books"], 2);
    let got: Got<PolkaCounts> = read_all(&POLKA, &file, "книги на даче").unwrap();
    assert_eq!(got.custom, vec![(4, 8), (4, 8)]);
    assert_eq!(got.records["books"], 2);
    // Приложение, которое этих записей не знает (с теми же метками), их пропускает.
    let other = Format { records: &[], ..POLKA };
    let skipped: Got<PolkaCounts> = read_all(&other, &file, "книги на даче").unwrap();
    assert!(skipped.custom.is_empty());
    assert_eq!(skipped.events.len(), 1);
}

#[test]
fn wrong_password_tampering_and_truncation() {
    let key = BackupKey::new_with("отпуск в Сочи", FAST).unwrap();
    let file = karman_file(&key);
    let check = |data: &[u8], pw: &str| read_all::<KarmanCounts>(&KARMAN, data, pw).map(|_| ());

    assert!(matches!(check(&file, "отпуск в Сочи!"), Err(Error::BackupPassword)));
    // Изменённый байт в середине — кусок не расшифруется.
    let mut bad = file.clone();
    let mid = bad.len() / 2;
    bad[mid] ^= 1;
    assert!(matches!(check(&bad, "отпуск в Сочи"), Err(Error::Backup(m)) if m.contains("повреждена")));
    // Подменённые параметры ключа в заголовке — ключ файла не расшифруется.
    let mut bad = file.clone();
    bad[15] = 2;
    assert!(matches!(check(&bad, "отпуск в Сочи"), Err(Error::BackupPassword)));
    // Обрезанный файл и файл без последнего куска.
    assert!(matches!(check(&file[..file.len() - 10], "отпуск в Сочи"), Err(Error::Backup(m)) if m.contains("обрывается")));
    assert!(check(&file[..file.len() - 1], "отпуск в Сочи").is_err());
    // Лишние байты в конце.
    let mut tail = file.clone();
    tail.push(0);
    assert!(matches!(check(&tail, "отпуск в Сочи"), Err(Error::Backup(m)) if m.contains("лишние")));
    // Переставленные куски.
    let first_len = (u32::from_le_bytes(file[HEADER..HEADER + 4].try_into().unwrap()) & !LAST) as usize;
    assert_eq!(first_len, CHUNK + TAG, "копия больше куска — кусков несколько");
    let a = HEADER..HEADER + 4 + first_len;
    let b_len = (u32::from_le_bytes(file[a.end..a.end + 4].try_into().unwrap()) & !LAST) as usize;
    let mut swapped = file[..HEADER].to_vec();
    swapped.extend_from_slice(&file[a.end..a.end + 4 + b_len]);
    swapped.extend_from_slice(&file[a.clone()]);
    swapped.extend_from_slice(&file[a.end + 4 + b_len..]);
    assert!(check(&swapped, "отпуск в Сочи").is_err());
    // Не копия вовсе, копия другого приложения и копия из будущей версии.
    assert!(matches!(check(b"PK\x03\x04 not a backup", "отпуск в Сочи"), Err(Error::Backup(m)) if m == "Это не резервная копия «Кармана»"));
    assert!(matches!(read_all::<PolkaCounts>(&POLKA, &file, "отпуск в Сочи"), Err(Error::Backup(m)) if m == "Это не резервная копия «Полки»"));
    let mut newer = file.clone();
    newer[8] = 9;
    assert!(matches!(check(&newer, "отпуск в Сочи"), Err(Error::Backup(m)) if m == "Копия сделана в более новой версии «Кармана» — обновите приложение"));
    // Огромные параметры ключа из чужого файла не принимаются.
    let mut greedy = file.clone();
    greedy[11..15].copy_from_slice(&(4u32 << 20).to_le_bytes());
    assert!(matches!(check(&greedy, "отпуск в Сочи"), Err(Error::Backup(m)) if m.contains("параметры")));
}

#[test]
fn key_meta_and_password_rules() {
    assert!(matches!(BackupKey::new_with("1234567", FAST), Err(Error::Backup(m)) if m.contains("не короче 8")));
    for weak in ["12345678", "87654321", "qwertyui", "йцукенгш", "abcdefgh", "aaaaaaaa", "abababab", "abcabcabc", "Пароль2026", "password123!", "1q2w3e4r", "карман 12", "11112222"] {
        assert!(matches!(check_new_password(&KARMAN, weak), Err(Error::Backup(m)) if m.contains("Слишком простой")), "{weak}");
    }
    for fine in ["отпуск в Сочи 2026", "пароль копий семьи", "длинный пароль", "correct horse battery", "Мой кот Барсик 7", "759301846215"] {
        assert!(check_new_password(&KARMAN, fine).is_ok(), "{fine}");
    }
    // Название своего приложения — простой пароль, чужого — нет.
    assert!(check_new_password(&POLKA, "полка 2026").is_err());
    assert!(check_new_password(&POLKA, "карман 12").is_ok());
    let key = BackupKey::new_with("длинный пароль", FAST).unwrap();
    let again = BackupKey::from_meta(&key.to_meta()).unwrap();
    assert_eq!((again.key, again.salt, again.kdf), (key.key, key.salt, key.kdf));
    assert!(BackupKey::from_meta("1:2:3").is_err());
    assert!(key.matches("длинный пароль").unwrap() && !key.matches("длинный пароль!").unwrap());
    // Сохранённым ключом копия открывается без пароля; ключ с другой солью — не подходит.
    let file = karman_file(&key);
    let mut r: Reader<_, KarmanCounts> = Reader::open_with_key(&KARMAN, Cursor::new(&file), &again).unwrap();
    while r.next_item::<Value>().unwrap().is_some() {}
    assert_eq!(r.blobs, 2);
    let other = BackupKey::new_with("длинный пароль", FAST).unwrap();
    assert!(matches!(Reader::<_, KarmanCounts>::open_with_key(&KARMAN, Cursor::new(&file), &other), Err(Error::BackupPassword)));
}

#[test]
fn is_backup_looks_at_the_mark() {
    let dir = std::env::temp_dir().join(format!("kit-backup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("copy.karman");
    std::fs::write(&path, karman_file(&BackupKey::new_with("отпуск в Сочи", FAST).unwrap())).unwrap();
    assert!(is_backup(&KARMAN, &path));
    assert!(!is_backup(&POLKA, &path));
    let _ = std::fs::remove_dir_all(&dir);
}

// ——— Копии, сделанные приложениями до общего модуля ———

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
}

/// Копия «Кармана» 0.15.1 (из его образцов форматов): 108 событий, 2 снимка чеков, 52 операции, 5 поездок.
#[test]
fn reads_a_backup_made_by_karman_0_15_1() {
    let file = fixture("karman-0.15.1.karman");
    assert!(matches!(read_all::<KarmanCounts>(&KARMAN, &file, "другой пароль копий"), Err(Error::BackupPassword)));
    let got: Got<KarmanCounts> = read_all(&KARMAN, &file, "отпуск в Сочи").unwrap();
    assert_eq!((got.manifest.app.as_str(), got.manifest.device_id.as_str()), ("0.15.1", "aaaa"));
    assert_eq!((got.events.len(), got.blobs.len()), (108, 2));
    assert_eq!((got.manifest.events, got.manifest.blobs), (108, 2));
    assert_eq!(got.manifest.counts, KarmanCounts { ops: 52, trips: 5 });
    assert!(got.events.iter().all(|e| e["device"] == "aaaa" && e["seq"].as_u64().is_some()));
}

/// Копия «Полки» 0.11.1 (из её образцов форматов): 10 событий, 2 обложки и файл книги одним куском.
#[test]
fn reads_a_backup_made_by_polka_0_11_1() {
    let file = fixture("polka-0.11.1.polka");
    let got: Got<PolkaCounts> = read_all(&POLKA, &file, "книги на даче").unwrap();
    assert_eq!((got.manifest.app.as_str(), got.manifest.device_id.as_str()), ("0.11.1", "aaaa"));
    assert_eq!((got.events.len(), got.blobs.len()), (10, 2));
    assert_eq!(got.manifest.counts.books, 1);
    assert_eq!(got.records["books"], 1);
    assert_eq!(got.custom.len(), 1);
    assert_eq!(got.custom[0].0, 4);
    // Без записей книг (как у «Кармана») та же копия тоже читается: кусок книги пропускается.
    let plain: Got<PolkaCounts> = read_all(&Format { records: &[], ..POLKA }, &file, "книги на даче").unwrap();
    assert!(plain.custom.is_empty());
}
