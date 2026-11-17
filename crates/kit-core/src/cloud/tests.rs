use super::*;
use crate::sync::tests::{Ev, Mem, F};
use serde_json::json;

const FAST: Kdf = Kdf { m_kib: 8 * 1024, t: 1, p: 1 };
const PW: &str = "отпуск в Сочи";

fn image(mime: &str, data: &[u8]) -> bool {
    !data.is_empty() && mime == "image/jpeg" && data.starts_with(&[0xFF, 0xD8, 0xFF])
}

/// Как у «Кармана» 0.15.
static KARMAN: Format = Format {
    names: AppNames { id: "karman", name: "Карман", name_acc: "Карман", name_gen: "Кармана", feminine: false },
    key_file: "karman.key",
    magic: *b"KRMC",
    events_ext: ".kev",
    max_seq: u64::MAX,
    blobs: Blobs { ext: ".kbl", max_file: (15 << 20) + 1024, what: "снимок чека", this: "этот снимок", of: "снимка", feminine: false, check: image },
    files: None,
};

/// Как у «Полки» 0.11: обложки и файлы книг кусками (здесь куски по 1000 байт).
static POLKA: Format = Format {
    names: AppNames { id: "polka", name: "Полка", name_acc: "Полку", name_gen: "Полки", feminine: true },
    key_file: "polka.key",
    magic: *b"PLKC",
    events_ext: ".pev",
    max_seq: i64::MAX as u64,
    blobs: Blobs { ext: ".pbl", max_file: (8 << 20) + 1024, what: "обложка", this: "эта обложка", of: "обложки", feminine: true, check: image },
    files: Some(Files { ext: ".pbk", part: 1000, what: "Файл книги" }),
};

impl CloudStore for Mem {
    fn origin(e: &Ev) -> (&str, u64) {
        (&e.device, e.seq)
    }
    fn events_range(&self, author: &str, from: u64, to: u64) -> Result<Vec<Ev>> {
        let mut v: Vec<Ev> = self.events.iter().filter(|e| e.device == author && (from..=to).contains(&e.seq)).cloned().collect();
        v.sort_by_key(|e| e.seq);
        Ok(v)
    }
    fn differs(&self, e: &Ev) -> Result<bool> {
        Ok(self.events.iter().any(|x| x.device == e.device && x.seq == e.seq && x.body != e.body))
    }
    fn present_blobs(&self) -> Result<Vec<String>> {
        Ok(self.blobs.keys().cloned().collect())
    }
}

fn jpeg(n: usize, fill: u8) -> Vec<u8> {
    let mut d = vec![fill; n];
    d[..3].copy_from_slice(&[0xFF, 0xD8, 0xFF]);
    d
}

/// Журнал с тремя записями; у первой — вложение.
fn demo(device: &str) -> Mutex<Mem> {
    let mut m = Mem::new(device);
    let pic = jpeg(50_000, 7);
    let id = blob_id(&pic);
    m.blobs.insert(id.clone(), ("image/jpeg".into(), pic));
    m.refs.push(id);
    for t in ["Пикник на обочине", "Лавр", "Щегол"] {
        m.add(json!({ "title": t }));
    }
    Mutex::new(m)
}

fn fresh(device: &str) -> Mutex<Mem> {
    Mutex::new(Mem::new(device))
}

fn add(s: &Mutex<Mem>, body: serde_json::Value) {
    lock(s).add(body);
}

fn round(cloud: &dyn Cloud, store: &Mutex<Mem>, key: &CloudKey, state: &mut State) -> Result<Report> {
    let cancel = AtomicBool::new(false);
    sync(&Round { cloud, store, key, now: 1_794_600_000, cancel: &cancel }, state, &mut |_| {})
}

fn versions(s: &Mutex<Mem>) -> BTreeMap<String, u64> {
    lock(s).versions().unwrap()
}

/// Записи у обоих одинаковые (порядок — по устройству и номеру).
fn same_data(a: &Mutex<Mem>, b: &Mutex<Mem>) {
    let sorted = |s: &Mutex<Mem>| {
        let mut v: Vec<(String, u64, serde_json::Value)> = lock(s).events.iter().map(|e| (e.device.clone(), e.seq, e.body.clone())).collect();
        v.sort_by(|x, y| (&x.0, x.1).cmp(&(&y.0, y.1)));
        v
    };
    assert_eq!(sorted(a), sorted(b));
}

#[test]
fn names_are_canonical() {
    assert_eq!(event_name(&KARMAN, "0a1b", 1, 500), "0a1b-1-500.kev");
    assert_eq!(parse_event_name(&KARMAN, "0a1b-1-500.kev"), Some(EventFileName { author: "0a1b".into(), from: 1, to: 500 }));
    for bad in ["0a1b-01-500.kev", "0a1b-0-5.kev", "0a1b-9-5.kev", "0a1b-1-5.kev.tmp", "a-b-1-5.kev", "-1-5.kev", "x/y-1-5.kev", "0a1b-1-5.pev", "karman.key"] {
        assert_eq!(parse_event_name(&KARMAN, bad), None, "{bad}");
    }
    // У «Полки» номера событий в базе — i64: больший номер в имени — чужой файл.
    assert!(parse_event_name(&KARMAN, "0a1b-1-9223372036854775808.kev").is_some());
    assert_eq!(parse_event_name(&POLKA, "0a1b-1-9223372036854775808.pev"), None);
    assert!(parse_event_name(&POLKA, "0a1b-1-9223372036854775807.pev").is_some());
    let key = CloudKey::generate(&POLKA).unwrap();
    let id = blob_id(b"cover");
    let name = key.blob_name(&id);
    assert!(name.ends_with(".pbl") && name.len() == 36);
    assert!(!name.contains(&id[..16]), "по имени не узнать отпечаток вложения");
    let part = key.file_name(&id, 2, 3);
    assert!(part.ends_with("-2-3.pbk") && parse_file_name(&POLKA, &part).is_some());
    assert_eq!(parse_file_name(&POLKA, &key.file_name(&id, 3, 3)), None, "кусок за пределами файла");
    assert_eq!((KARMAN.dirs(), POLKA.dirs()), (vec![Dir::Events, Dir::Blobs], vec![Dir::Events, Dir::Blobs, Dir::Files]));
}

#[test]
fn key_file_opens_only_with_the_password() {
    let mk = CloudKey::generate(&KARMAN).unwrap();
    let kf = KeyFile::parse(&KARMAN, &mk.key_file(PW, FAST, 1).unwrap().to_bytes()).unwrap();
    let again = CloudKey::from_key_file(&KARMAN, &kf, PW).unwrap();
    assert_eq!((again.key, again.group()), (mk.key, mk.group()));
    assert!(matches!(CloudKey::from_key_file(&KARMAN, &kf, "отпуск в Сочи!"), Err(Error::CloudPassword)));
    // Ключ одного приложения не открывается другим: строки ключей свои.
    assert!(matches!(CloudKey::from_key_file(&POLKA, &kf, PW), Err(Error::CloudPassword)));
    // Подменить номер облака или параметры незаметно нельзя.
    let mut other = kf.clone();
    other.group = "0".repeat(32);
    assert!(matches!(CloudKey::from_key_file(&KARMAN, &other, PW), Err(Error::CloudPassword)));
    let mut weaker = kf.clone();
    weaker.t = 2;
    assert!(matches!(CloudKey::from_key_file(&KARMAN, &weaker, PW), Err(Error::CloudPassword)));
    // Огромные параметры из чужого файла не считаются.
    let mut greedy = kf.clone();
    greedy.m = 4 << 20;
    assert!(matches!(CloudKey::from_key_file(&KARMAN, &greedy, PW), Err(Error::Cloud(CloudFail::Format, _))));
    assert!(matches!(KeyFile::parse(&KARMAN, b"{}"), Err(Error::Cloud(CloudFail::Format, _))));
    let mut newer = kf.clone();
    newer.format = 9;
    let e = KeyFile::parse(&KARMAN, &newer.to_bytes()).unwrap_err();
    assert!(matches!(e, Error::Cloud(CloudFail::Newer, ref m) if m == "Облако подключено более новой версией «Кармана» — обновите приложение"), "{e}");
    assert!(matches!(mk.key_file("1234567", FAST, 1), Err(Error::Backup(m)) if m.contains("не короче 8")));
    // Ключ на устройстве.
    let back = CloudKey::from_meta(&KARMAN, &mk.to_meta()).unwrap();
    assert_eq!((back.key, back.group()), (mk.key, mk.group()));
    assert!(CloudKey::from_meta(&KARMAN, "1:abc:xyz").is_err());
}

#[test]
fn two_devices_exchange_events_and_blobs() {
    for fmt in [&KARMAN, &POLKA] {
        let cloud = MemCloud::default();
        let a = demo("aaaa");
        let b = fresh("bbbb");
        let (ka, created) = join(fmt, &cloud, PW, FAST, 1).unwrap();
        assert!(created);
        assert!(cloud.raw(Dir::Root, fmt.key_file).is_some());
        assert!(matches!(join(fmt, &cloud, "чужой пароль", FAST, 1), Err(Error::CloudPassword)));
        let (kb, created) = join(fmt, &cloud, PW, FAST, 2).unwrap();
        assert!(!created);
        assert_eq!(ka.group(), kb.group());

        let (mut sa, mut sb) = (State::default(), State::default());
        let r = round(&cloud, &a, &ka, &mut sa).unwrap();
        assert_eq!((r.received, r.sent, r.files_sent), (0, 3, 1));
        assert!(cloud.names(Dir::Events).iter().all(|n| n.ends_with(fmt.events_ext)));
        assert!(cloud.raw(Dir::Events, &cloud.names(Dir::Events)[0]).unwrap().starts_with(&fmt.magic));
        // B знает, что ему нужно вложение (запись на него ссылается).
        lock(&b).refs = lock(&a).refs.clone();
        let r = round(&cloud, &b, &kb, &mut sb).unwrap();
        assert_eq!((r.received, r.sent, r.files_received, r.files_waiting), (3, 0, 1, 0));
        let r = round(&cloud, &a, &ka, &mut sa).unwrap();
        assert_eq!((r.received, r.sent), (0, 0));
        same_data(&a, &b);
        assert!(lock(&b).missing_blobs().unwrap().is_empty(), "вложение пришло");

        // Запись на одном устройстве появляется на другом.
        add(&b, json!({ "rating": 9 }));
        assert_eq!(round(&cloud, &b, &kb, &mut sb).unwrap().sent, 1);
        assert_eq!(round(&cloud, &a, &ka, &mut sa).unwrap().received, 1);
        // Без изменений обмен ничего не гоняет.
        let r = round(&cloud, &a, &ka, &mut sa).unwrap();
        assert_eq!((r.received, r.sent, r.files_sent, r.files_received), (0, 0, 0, 0));
        same_data(&a, &b);
    }
}

#[test]
fn records_from_wifi_go_through_the_cloud_and_are_not_sent_twice() {
    let cloud = MemCloud::default();
    let a = demo("aaaa");
    let b = fresh("bbbb");
    let c = fresh("cccc");
    add(&c, json!({ "title": "Брат" }));
    // C связан с B только по Wi-Fi.
    let to_b = lock(&c).events_missing(&versions(&b)).unwrap();
    lock(&b).apply_remote(&to_b).unwrap();
    let (k, _) = join(&KARMAN, &cloud, PW, FAST, 1).unwrap();
    let (mut sa, mut sb, mut sc) = (State::default(), State::default(), State::default());
    round(&cloud, &a, &k, &mut sa).unwrap();
    round(&cloud, &b, &k, &mut sb).unwrap();
    round(&cloud, &a, &k, &mut sa).unwrap();
    assert!(versions(&a).contains_key("cccc"), "записи C пришли через B");
    // Потом C подключает облако сам: своё он уже не отправляет.
    let r = round(&cloud, &c, &k, &mut sc).unwrap();
    assert_eq!(r.sent, 0);
    same_data(&a, &c);
}

#[test]
fn small_files_are_merged_and_a_new_device_gets_everything() {
    let cloud = MemCloud::default();
    let a = fresh("aaaa");
    let (k, _) = join(&POLKA, &cloud, PW, FAST, 1).unwrap();
    let mut sa = State::default();
    for i in 0..20 {
        add(&a, json!({ "k": i }));
        round(&cloud, &a, &k, &mut sa).unwrap();
    }
    let names = cloud.names(Dir::Events);
    assert!(names.len() <= COMPACT_AFTER + 1, "мелкие файлы слиты: {names:?}");
    assert!(names.iter().any(|n| n.starts_with("aaaa-1-")), "{names:?}");
    let b = fresh("bbbb");
    round(&cloud, &b, &k, &mut State::default()).unwrap();
    assert_eq!(versions(&b).get("aaaa"), Some(&20));
    same_data(&a, &b);
}

#[test]
fn a_gap_in_the_cloud_is_not_applied_and_heals() {
    let cloud = MemCloud::default();
    let a = fresh("aaaa");
    for i in 0..3 {
        add(&a, json!({ "s": i }));
    }
    let (k, _) = join(&KARMAN, &cloud, PW, FAST, 1).unwrap();
    let mut sa = State::default();
    round(&cloud, &a, &k, &mut sa).unwrap(); // aaaa-1-3
    for i in 0..2 {
        add(&a, json!({ "g": i }));
        round(&cloud, &a, &k, &mut sa).unwrap(); // aaaa-4-4, aaaa-5-5
    }
    cloud.remove(Dir::Events, &event_name(&KARMAN, "aaaa", 4, 4));
    let b = fresh("bbbb");
    let mut sb = State::default();
    round(&cloud, &b, &k, &mut sb).unwrap();
    assert_eq!(versions(&b).get("aaaa"), Some(&3), "после пропуска ничего не применяется");
    // Устройство A видит пропуск и отправляет недостающее снова.
    assert_eq!(round(&cloud, &a, &k, &mut sa).unwrap().sent, 1);
    round(&cloud, &b, &k, &mut sb).unwrap();
    assert_eq!(versions(&b).get("aaaa"), Some(&5));
}

#[test]
fn tampered_renamed_and_foreign_files_are_skipped() {
    for fmt in [&KARMAN, &POLKA] {
        let cloud = MemCloud::default();
        let a = demo("aaaa");
        let (k, _) = join(fmt, &cloud, PW, FAST, 1).unwrap();
        let mut sa = State::default();
        round(&cloud, &a, &k, &mut sa).unwrap();
        let name = cloud.names(Dir::Events)[0].clone();
        let span = parse_event_name(fmt, &name).unwrap();
        // Изменённый байт.
        let mut bad = cloud.raw(Dir::Events, &name).unwrap();
        let mid = bad.len() / 2;
        bad[mid] ^= 1;
        cloud.set_raw(Dir::Events, &name, bad);
        let b = fresh("bbbb");
        let mut sb = State::default();
        let r = round(&cloud, &b, &k, &mut sb).unwrap();
        assert_eq!(r.received, 0);
        assert!(r.problems.iter().any(|p| p.contains("файл в облаке повреждён или подменён") && p.contains("удалён")), "{:?}", r.problems);
        assert!(cloud.raw(Dir::Events, &name).is_none(), "испорченный файл удалён");
        // Устройство A видит пропуск и отправляет записи заново — и всё сходится.
        assert_eq!(round(&cloud, &a, &k, &mut sa).unwrap().sent, 3);
        let r = round(&cloud, &b, &k, &mut sb).unwrap();
        assert!(r.received == 3 && r.problems.is_empty(), "{r:?}");
        // Тот же файл под другим именем (будто в нём больше событий) не расшифруется.
        let good = cloud.raw(Dir::Events, &name).unwrap();
        let fake = event_name(fmt, &span.author, span.from, span.to + 100);
        cloud.set_raw(Dir::Events, &fake, good);
        let c = fresh("cccc");
        let r = round(&cloud, &c, &k, &mut State::default()).unwrap();
        assert!(r.problems.iter().any(|p| p.contains(&fake)), "{:?}", r.problems);
        assert!(cloud.raw(Dir::Events, &fake).is_none());
        assert_eq!(versions(&c).get("aaaa"), versions(&a).get("aaaa"));
        // Вложение, подменённое другим снимком, не принимается и удаляется; A отправит его снова.
        let blob = cloud.names(Dir::Blobs)[0].clone();
        let (_, other) = seal_blob(&k, &blob_id(&jpeg(10, 1)), "image/jpeg", &jpeg(10, 1)).unwrap();
        cloud.set_raw(Dir::Blobs, &blob, other);
        let d = fresh("dddd");
        lock(&d).refs = lock(&a).refs.clone();
        let mut sd = State::default();
        let r = round(&cloud, &d, &k, &mut sd).unwrap();
        assert_eq!((r.files_received, r.files_waiting), (0, 1));
        let damaged = format!("{} в облаке {}", fmt.blobs.what, if fmt.blobs.feminine { "повреждена" } else { "повреждён" });
        assert!(r.problems.iter().any(|p| p.contains(&damaged)), "{:?}", r.problems);
        assert!(cloud.raw(Dir::Blobs, &blob).is_none());
        sa.blobs_listed = 0; // раз в сутки A сверяет список вложений в облаке
        assert_eq!(round(&cloud, &a, &k, &mut sa).unwrap().files_sent, 1);
        assert_eq!(round(&cloud, &d, &k, &mut sd).unwrap().files_received, 1);
    }
}

/// Файл одного приложения другим не читается и называется понятно.
#[test]
fn files_of_another_app_are_not_ours() {
    let pk = CloudKey::generate(&POLKA).unwrap();
    let kk = CloudKey { fmt: &KARMAN, group: pk.group.clone(), key: pk.key };
    let (name, data) = seal_events(&pk, "aaaa", 1, 1, &[Ev { device: "aaaa".into(), seq: 1, body: json!({}) }]).unwrap();
    let e = open_events::<Mem>(&kk, "aaaa", 1, 1, &data).unwrap_err();
    assert!(e.to_string().starts_with("файл в облаке не от «Кармана» или повреждён"), "{e}");
    assert!(open_events::<Mem>(&pk, "aaaa", 1, 1, &data).is_ok(), "{name}");
    let (bname, blob) = seal_blob(&kk, &blob_id(&jpeg(10, 1)), "image/jpeg", &jpeg(10, 1)).unwrap();
    let e = open_blob(&pk, &blob_id(&jpeg(10, 1)), &bname, &blob).unwrap_err();
    assert_eq!(e.to_string(), format!("обложка в облаке не от «Полки» или повреждена ({bname})"));
    // Из более новой версии — просьба обновиться.
    let mut newer = data.clone();
    newer[4] = 2;
    let e = open_events::<Mem>(&pk, "aaaa", 1, 1, &newer).unwrap_err();
    assert!(matches!(e, Error::Cloud(CloudFail::Newer, ref m) if m == "В облаке файлы более новой версии «Полки» — обновите приложение"), "{e}");
}

#[test]
fn deleted_or_reconnected_cloud_stops_the_exchange() {
    let cloud = MemCloud::default();
    let a = fresh("aaaa");
    for i in 0..3 {
        add(&a, json!({ "s": i }));
    }
    let (k, _) = join(&KARMAN, &cloud, PW, FAST, 1).unwrap();
    let mut sa = State::default();
    round(&cloud, &a, &k, &mut sa).unwrap();
    // Облако подключили заново на другом устройстве — с новым ключом.
    let cancel = AtomicBool::new(false);
    wipe(&KARMAN, &cloud, &cancel).unwrap();
    assert!(cloud.names(Dir::Events).is_empty() && cloud.raw(Dir::Root, KARMAN.key_file).is_none());
    let e = round(&cloud, &a, &k, &mut sa).unwrap_err();
    assert!(matches!(e, Error::Cloud(CloudFail::Gone, ref m) if m.starts_with("Данные «Кармана» в облаке удалены")), "{e}");
    let (k2, created) = join(&KARMAN, &cloud, PW, FAST, 2).unwrap();
    assert!(created);
    assert!(matches!(round(&cloud, &a, &k, &mut sa), Err(Error::Cloud(CloudFail::Replaced, _))));
    // По паролю устройство получает новый ключ и снова обменивается.
    let (k3, created) = join(&KARMAN, &cloud, PW, FAST, 3).unwrap();
    assert!(!created);
    assert_eq!(k3.group(), k2.group());
    assert_eq!(round(&cloud, &a, &k3, &mut State::default()).unwrap().sent, 3);
    // Новый пароль копий: ключ облака тот же, открывается новым паролем.
    let kf = k3.key_file("новый пароль копий", FAST, 4).unwrap();
    put_key_file(&cloud, &k3, &kf).unwrap();
    assert!(matches!(join(&KARMAN, &cloud, PW, FAST, 5), Err(Error::CloudPassword)));
    let (k4, _) = join(&KARMAN, &cloud, "новый пароль копий", FAST, 5).unwrap();
    assert_eq!(k4.key, k3.key);
    assert!(matches!(put_key_file(&cloud, &k, &k.key_file(PW, FAST, 6).unwrap()), Err(Error::Cloud(CloudFail::Replaced, _))));
}

#[test]
fn only_our_files_are_deleted() {
    let cloud = MemCloud::default();
    let a = demo("aaaa");
    let (k, _) = join(&POLKA, &cloud, PW, FAST, 1).unwrap();
    round(&cloud, &a, &k, &mut State::default()).unwrap();
    // Свои файлы человека в папках приложения.
    for (d, n) in [(Dir::Events, "заметки.txt"), (Dir::Blobs, "фото.jpg"), (Dir::Events, "aaaa-01-3.pev"), (Dir::Root, "список.docx"), (Dir::Files, "книга.pdf")] {
        cloud.set_raw(d, n, b"my file".to_vec());
    }
    let cancel = AtomicBool::new(false);
    wipe(&POLKA, &cloud, &cancel).unwrap();
    assert_eq!(cloud.names(Dir::Events), ["aaaa-01-3.pev", "заметки.txt"]);
    assert_eq!(cloud.names(Dir::Blobs), ["фото.jpg"]);
    assert_eq!(cloud.names(Dir::Root), ["список.docx"]);
    assert_eq!(cloud.names(Dir::Files), ["книга.pdf"]);
    // Подключение заново тоже убирает только файлы приложения.
    round(&cloud, &a, &join(&POLKA, &cloud, PW, FAST, 2).unwrap().0, &mut State::default()).unwrap();
    cloud.remove(Dir::Root, POLKA.key_file);
    join(&POLKA, &cloud, PW, FAST, 3).unwrap();
    assert_eq!(cloud.names(Dir::Events), ["aaaa-01-3.pev", "заметки.txt"]);
    assert_eq!(cloud.names(Dir::Blobs), ["фото.jpg"]);
    // У «Кармана» папки больших файлов нет: её и не смотрим.
    let calls = cloud.calls.load(Ordering::Relaxed);
    wipe(&KARMAN, &cloud, &cancel).unwrap();
    assert_eq!(cloud.calls.load(Ordering::Relaxed) - calls, 3, "ключ, список e и список b — папку k не смотрим");
}

#[test]
fn same_device_number_with_other_records_is_a_conflict() {
    let cloud = MemCloud::default();
    let a = fresh("aaaa");
    add(&a, json!({ "goal": 24 }));
    let (k, _) = join(&KARMAN, &cloud, PW, FAST, 1).unwrap();
    round(&cloud, &a, &k, &mut State::default()).unwrap();
    // База начата заново под тем же номером устройства и с другой записью.
    let again = fresh("aaaa");
    add(&again, json!({ "goal": 52 }));
    assert!(matches!(round(&cloud, &again, &k, &mut State::default()), Err(Error::Cloud(CloudFail::Conflict, _))));
    assert_eq!(lock(&again).events[0].body, json!({ "goal": 52 }), "база не тронута");
    // Та же база, что отправляла, расхождений не находит.
    assert!(round(&cloud, &a, &k, &mut State::default()).is_ok());
}

#[test]
fn cancel_stops_between_requests() {
    let cloud = MemCloud::default();
    let a = demo("aaaa");
    let (k, _) = join(&KARMAN, &cloud, PW, FAST, 1).unwrap();
    let cancel = AtomicBool::new(true);
    let r = sync(&Round { cloud: &cloud, store: &a, key: &k, now: 1, cancel: &cancel }, &mut State::default(), &mut |_| {});
    assert!(matches!(r, Err(Error::Cloud(CloudFail::Cancelled, _))));
    assert!(cloud.names(Dir::Events).is_empty());
}

/// Большие файлы через облако: кусками, только где включён обмен ими, с ожиданием недостающих кусков.
#[test]
fn big_files_go_through_the_cloud_in_parts() {
    let cloud = MemCloud::default();
    let (key, _) = join(&POLKA, &cloud, PW, FAST, 1).unwrap();
    let a = demo("aaaa");
    let b = fresh("bbbb");
    let data: Vec<u8> = (0..2007u32).map(|i| (i % 241) as u8).collect();
    lock(&a).files.insert("book".into(), data.clone());
    let (mut sa, mut sb) = (State::default(), State::default());
    // Обмен файлами выключен: в облаке только записи.
    round(&cloud, &a, &key, &mut sa).unwrap();
    assert!(cloud.names(Dir::Files).is_empty());
    lock(&a).files_on = true;
    let r = round(&cloud, &a, &key, &mut sa).unwrap();
    assert_eq!(r.whole_sent, 1);
    let parts = cloud.names(Dir::Files);
    assert_eq!(parts.len(), 3, "{parts:?}");
    assert!(parts.iter().all(|n| n.ends_with("-3.pbk")));
    // Второму устройству нужен файл; один кусок ещё не дошёл — ждём.
    {
        let mut m = lock(&b);
        m.files_on = true;
        m.wanted.push(F { id: "book".into(), size: data.len() as u64 });
    }
    let last = parts.iter().find(|n| n.contains("-2-3")).unwrap().clone();
    let saved = cloud.raw(Dir::Files, &last).unwrap();
    cloud.remove(Dir::Files, &last);
    let r = round(&cloud, &b, &key, &mut sb).unwrap();
    assert_eq!((r.whole_received, r.whole_waiting), (0, 1));
    cloud.set_raw(Dir::Files, &last, saved);
    let r = round(&cloud, &b, &key, &mut sb).unwrap();
    assert_eq!((r.whole_received, r.whole_waiting), (1, 0), "{r:?}");
    assert_eq!(lock(&b).files.get("book"), Some(&data));
    // Испорченный кусок — понятная запись в отчёте, файл не собран.
    let c = fresh("cccc");
    {
        let mut m = lock(&c);
        m.files_on = true;
        m.wanted.push(F { id: "book".into(), size: data.len() as u64 });
    }
    let mut bad = cloud.raw(Dir::Files, &parts[0]).unwrap();
    bad[40] ^= 1;
    cloud.set_raw(Dir::Files, &parts[0], bad);
    let r = round(&cloud, &c, &key, &mut State::default()).unwrap();
    assert_eq!(r.whole_received, 0);
    assert!(r.problems.iter().any(|p| p.starts_with("Файл книги «book»: файл в облаке повреждён или подменён")), "{:?}", r.problems);
    // У «Кармана» больших файлов нет: шаг пропускается, даже если база их предлагает.
    let cloud2 = MemCloud::default();
    let (k2, _) = join(&KARMAN, &cloud2, PW, FAST, 1).unwrap();
    let r = round(&cloud2, &a, &k2, &mut State::default()).unwrap();
    assert_eq!(r.whole_sent, 0);
    assert!(cloud2.names(Dir::Files).is_empty());
}

#[test]
fn gaps_and_chunks() {
    let s = |from, to| Span { from, to };
    assert_eq!(gaps(&[], 3), vec![(1, 3)]);
    assert_eq!(gaps(&[s(1, 3)], 3), vec![]);
    assert_eq!(gaps(&[s(1, 3), s(6, 8)], 10), vec![(4, 5), (9, 10)]);
    assert_eq!(gaps(&[s(2, 9), s(1, 4)], 9), vec![]);
    assert_eq!(gaps(&[s(5, 20)], 3), vec![(1, 3)]);
    assert_eq!(chunks(1, 12_345), vec![s(1, 5000), s(5001, 10_000), s(10_001, 12_345)]);
}
