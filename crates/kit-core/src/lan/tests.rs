use super::*;
use crate::sync::tests::{Mem, KARMAN, POLKA};
use serde_json::json;

const POLKA_NAMES: AppNames = AppNames { id: "polka", name: "Полка", name_acc: "Полку", name_gen: "Полки", feminine: true };
const KARMAN_NAMES: AppNames = AppNames { id: "karman", name: "Карман", name_acc: "Карман", name_gen: "Кармана", feminine: false };

/// Как у «Полки» 0.11.
const POLKA_LAN: Lan = Lan {
    names: POLKA_NAMES,
    profile: &POLKA,
    tcp_port: 47510,
    discovery_port: 47511,
    magic: *b"PLKL",
    query_magic: *b"PLKQ",
    answer_magic: *b"PLKA",
    joiner: b"polka-joiner",
    host: b"polka-host",
    personal: b"polka-lan",
    not_ours: "это не «Полка»",
    qr_scheme: "polka://pair?",
    qr_version: 1,
};

/// Как у «Кармана» 0.15.
const KARMAN_LAN: Lan = Lan {
    names: KARMAN_NAMES,
    profile: &KARMAN,
    tcp_port: 47500,
    discovery_port: 47501,
    magic: *b"KRML",
    query_magic: *b"KRMQ",
    answer_magic: *b"KRMA",
    joiner: b"karman-joiner",
    host: b"karman-host",
    personal: b"karman-lan",
    not_ours: "это не «Карман» 0.15 или новее",
    qr_scheme: "karman://pair?",
    qr_version: 2,
};

fn hex(b: [u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Ключи выводятся как в «Полке» 0.11 и «Кармане» 0.15: значения взяты из их проверок
/// (`keys_are_derived_as_before`) и не меняются — иначе устройства с прежней версией
/// перестанут узнавать друг друга.
#[test]
fn keys_are_derived_as_in_both_apps() {
    let k = [0x11u8; 32];
    let (nc, ns) = ([0x22u8; 32], [0x33u8; 32]);
    let cases: [(&Lan, [&str; 7], &[u8]); 2] = [
        (
            &POLKA_LAN,
            [
                "0bdaaec39968032b1d3d4f267f1283d34bce5504340e6f3a28544770a4c27a43",
                "70b0495f76d7f0ce7d789cdf83ee28fbf7d42d872a3329e409d1f121693e8be1",
                "16549426e96bd48161ccb91e13103c2ce800be7b821bc73000a5cb8d304def46",
                "051dad75907b35d30c34c1a660deabf656210bee5fe2ff0536b2b152f661c5ff",
                "b065d8f30d2ab476e3b829a2775d6c07f28c5599459608a9deb201a39d6e0a8d",
                "af63aedcbe1f33b2c858987f74ac46bdbc42ec950e870bc6c796ac5977c87733",
                "2d59f99b83a396f9cad53910b14e0e0832a68bd959e01b347402528a6121494e",
            ],
            b"47510",
        ),
        (
            &KARMAN_LAN,
            [
                "aabab280e54c704ab6e05232726f10bc96b5c82962d0e967ddc508ad014dfc1f",
                "2759a4300e7305a0a46f3352be33d0c35fa3bcece1ecb601f9af51405d044ba7",
                "7193ad7b9e7c6666ca83b8a779f10695bbe7b7ede7eee426d13ee62e304b59b1",
                "0aa9e7686201ec9e2dc5eff40091c196097b41b049d03fcf49e7b78f300b5921",
                "9edd66e3dae789177ee32b8c9b9bd081ebda5b6657505124dafb3529ecfe31b7",
                "f24f3f3795362077b87008ce52825d5f56315dd5528b2e566514fff9d4f2f7a1",
                "a88d68b9754fc83de670a3507cbd17010fa24dd0bb8803085f126ce9e1dab01f",
            ],
            b"47500",
        ),
    ];
    for (lan, want, port) in cases {
        let got = [
            lan.mac(&k, &[b"pair key"]),
            lan.mac(&k, &[b"joiner confirm"]),
            lan.mac(&k, &[b"host confirm"]),
            lan.mac(&k, &[b"sync c2s", &nc, &ns]),
            lan.mac(&k, &[b"sync s2c", &nc, &ns]),
            lan.mac(&k, &[b"query", b"aaaa", b"bbbb", &nc]),
            lan.mac(&k, &[b"answer", b"bbbb", b"aaaa", &nc, port]),
        ];
        assert_eq!(got.map(hex), want.map(String::from), "{}", lan.names.name);
    }
}

type Log = Arc<Mutex<Vec<(String, std::result::Result<SyncReport, String>)>>>;

struct Side {
    store: Arc<Mutex<Mem>>,
    host: Host<Mem>,
    keys: Arc<Mutex<BTreeMap<String, [u8; 32]>>>,
    log: Log,
}

fn side(lan: &'static Lan, device: &str, name: &str) -> Side {
    let store = Arc::new(Mutex::new(Mem::new(device)));
    let keys: Arc<Mutex<BTreeMap<String, [u8; 32]>>> = Arc::default();
    let log: Log = Arc::default();
    let (k1, k2, l) = (keys.clone(), keys.clone(), log.clone());
    let host = Host {
        lan,
        store: store.clone(),
        device: device.into(),
        name: name.into(),
        code: Arc::default(),
        peer_key: Arc::new(move |id| lock(&k1).get(id).copied()),
        paired: Arc::new(move |p, k| {
            lock(&k2).insert(p.device.clone(), *k);
            Ok(())
        }),
        synced: Arc::new(move |id, r| lock(&l).push((id.to_string(), r.map_err(|e| e.to_string())))),
    };
    Side { store, host, keys, log }
}

fn put(s: &Side, title: &str) {
    lock(&s.store).add(json!({ "title": title }));
}

fn count(s: &Side) -> usize {
    lock(&s.store).events.len()
}

fn local(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// Ждём, пока сервер в своём потоке допишет журнал.
fn wait_log(s: &Side, n: usize) {
    let t0 = Instant::now();
    while lock(&s.log).len() < n && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn remember(keys: &Arc<Mutex<BTreeMap<String, [u8; 32]>>>) -> impl Fn(&Peer, &[u8; 32]) -> std::result::Result<(), String> {
    let keys = keys.clone();
    move |p, k| {
        lock(&keys).insert(p.device.clone(), *k);
        Ok(())
    }
}

#[test]
fn pair_by_code_then_sync_by_pair_key() {
    for lan in [&POLKA_LAN, &KARMAN_LAN] {
        let laptop = side(lan, "laptop", "Ноутбук");
        let phone = side(lan, "phone", "Телефон");
        put(&laptop, "Пикник на обочине");
        put(&phone, "Лавр");
        let server = Server::start(laptop.host.clone(), 0).unwrap();
        *lock(&laptop.host.code) = Some(CodeState::new("6WVGHEWK".into()));
        let (peer, r) = pair_join(lan, local(server.port), "6wvg-hewk", &*phone.store, "phone", "Телефон", &remember(&phone.keys)).unwrap();
        assert_eq!((peer.device.as_str(), peer.name.as_str(), peer.app.as_str()), ("laptop", "Ноутбук", lan.profile.version));
        assert_eq!((r.sent, r.received), (1, 1), "первый обмен — сразу при связи");
        wait_log(&laptop, 1);
        let kl = lock(&laptop.keys).get("phone").copied().expect("ноутбук запомнил телефон");
        let kp = lock(&phone.keys).get("laptop").copied().expect("телефон запомнил ноутбук");
        assert_eq!(kl, kp, "ключ пары у обоих один");
        assert!(lock(&laptop.host.code).is_none(), "код сработал один раз");
        assert_eq!(count(&laptop), 2);
        let (who, rep) = lock(&laptop.log)[0].clone();
        assert_eq!((who.as_str(), rep.unwrap().peer_name.as_str()), ("phone", "Телефон"));
        // Код больше не действует.
        let again = pair_join(lan, local(server.port), "6WVGHEWK", &*phone.store, "phone", "Телефон", &|_, _| Ok(())).unwrap_err();
        assert!(matches!(again, Error::Lan(LanFail::NoCode, _)), "{again}");

        // Обмен по ключу пары — в обе стороны, повторный ничего не передаёт.
        put(&phone, "Брат");
        let r = sync_peer(lan, local(server.port), "phone", "laptop", "Ноутбук", &kp, &*phone.store, "Телефон").unwrap();
        assert_eq!((r.peer.as_str(), r.sent, r.received), ("laptop", 1, 0));
        let r = sync_peer(lan, local(server.port), "phone", "laptop", "Ноутбук", &kp, &*phone.store, "Телефон").unwrap();
        assert_eq!((r.sent, r.received), (0, 0));
        // Чужой ключ: ни одной записи, понятная ошибка.
        put(&phone, "Щегол");
        let bad = sync_peer(lan, local(server.port), "phone", "laptop", "Ноутбук", &[7u8; 32], &*phone.store, "Телефон").unwrap_err();
        assert!(matches!(bad, Error::Lan(LanFail::NotPeer, _) | Error::Io(_)), "{bad}");
        assert_eq!(count(&laptop), 3, "с чужим ключом ничего не пришло");
        // Не связанное устройство и не тот адресат.
        let unknown = sync_peer(lan, local(server.port), "stranger", "laptop", "Ноутбук", &kp, &*phone.store, "Чужой").unwrap_err();
        assert!(matches!(unknown, Error::Lan(LanFail::Unknown, ref m) if m.contains("«Ноутбук» не узнало")), "{unknown}");
        let other = sync_peer(lan, local(server.port), "phone", "desktop", "Компьютер", &kp, &*phone.store, "Телефон").unwrap_err();
        assert!(matches!(other, Error::Lan(LanFail::NotPeer, _)), "{other}");
        server.stop();
    }
}

/// Большие файлы «Полки» идут по Wi-Fi кусками — протокол приложения доходит до обмена.
#[test]
fn files_travel_over_the_session() {
    let laptop = side(&POLKA_LAN, "laptop", "Ноутбук");
    let phone = side(&POLKA_LAN, "phone", "Телефон");
    let data: Vec<u8> = (0..2500u32).map(|i| (i % 251) as u8).collect();
    {
        let mut l = lock(&laptop.store);
        l.files_on = true;
        l.files.insert("book".into(), data.clone());
    }
    {
        let mut p = lock(&phone.store);
        p.files_on = true;
        p.wanted.push(crate::sync::tests::F { id: "book".into(), size: data.len() as u64 });
    }
    let server = Server::start(laptop.host.clone(), 0).unwrap();
    *lock(&laptop.host.code) = Some(CodeState::new("6WVGHEWK".into()));
    let (_, r) = pair_join(&POLKA_LAN, local(server.port), "6WVGHEWK", &*phone.store, "phone", "Телефон", &|_, _| Ok(())).unwrap();
    assert_eq!(r.whole_received, 1, "{r:?}");
    assert_eq!(lock(&phone.store).files.get("book"), Some(&data));
    server.stop();
}

#[test]
fn five_wrong_codes_cancel_the_code() {
    let laptop = side(&POLKA_LAN, "laptop", "Ноутбук");
    let phone = side(&POLKA_LAN, "phone", "Телефон");
    let server = Server::start(laptop.host.clone(), 0).unwrap();
    *lock(&laptop.host.code) = Some(CodeState::new("6WVGHEWK".into()));
    for left in (0..CODE_TRIES).rev() {
        let e = pair_join(&POLKA_LAN, local(server.port), "6WVG-HEWM", &*phone.store, "phone", "Телефон", &|_, _| panic!("не должно связаться")).unwrap_err();
        assert!(matches!(e, Error::Lan(LanFail::WrongCode(l), _) if l == left), "{e}");
    }
    let st = lock(&laptop.host.code).clone().unwrap();
    assert_eq!((st.left, st.wrong), (0, 5));
    // И верный код больше не проходит.
    let e = pair_join(&POLKA_LAN, local(server.port), "6WVGHEWK", &*phone.store, "phone", "Телефон", &|_, _| panic!()).unwrap_err();
    assert!(matches!(e, Error::Lan(LanFail::Cancelled, ref m) if m.contains("пять неверных")), "{e}");
    assert!(lock(&laptop.keys).is_empty() && count(&laptop) == 0);
    // Истёкший код — как не показанный.
    *lock(&laptop.host.code) = Some(CodeState { until: Instant::now() - Duration::from_secs(1), ..CodeState::new("6WVGHEWK".into()) });
    let e = pair_join(&POLKA_LAN, local(server.port), "6WVGHEWK", &*phone.store, "phone", "Телефон", &|_, _| panic!()).unwrap_err();
    assert!(matches!(e, Error::Lan(LanFail::NoCode, _)), "{e}");
    server.stop();
}

/// Подставное устройство, которое не знает кода: связь не проходит, и оно ничего не получает.
#[test]
fn fake_host_without_code_gets_nothing() {
    let fake = side(&KARMAN_LAN, "fake", "Ноутбук");
    let phone = side(&KARMAN_LAN, "phone", "Телефон");
    put(&phone, "Секретная запись");
    let server = Server::start(fake.host.clone(), 0).unwrap();
    // У подставного свой код — человек вводит код с настоящего ноутбука.
    *lock(&fake.host.code) = Some(CodeState::new("ABCDEFGH".into()));
    let e = pair_join(&KARMAN_LAN, local(server.port), "6WVGHEWK", &*phone.store, "phone", "Телефон", &|_, _| panic!()).unwrap_err();
    assert!(matches!(e, Error::Lan(LanFail::WrongCode(4), _)), "{e}");
    assert!(count(&fake) == 0 && lock(&phone.keys).is_empty());
    server.stop();
}

/// Два приложения в одной сети не связываются: свои метки и ключи.
#[test]
fn apps_do_not_talk_to_each_other() {
    let laptop = side(&KARMAN_LAN, "laptop", "Ноутбук");
    let phone = side(&POLKA_LAN, "phone", "Телефон");
    let server = Server::start(laptop.host.clone(), 0).unwrap();
    *lock(&laptop.host.code) = Some(CodeState::new("6WVGHEWK".into()));
    let e = pair_join(&POLKA_LAN, local(server.port), "6WVGHEWK", &*phone.store, "phone", "Телефон", &|_, _| panic!()).unwrap_err();
    assert!(matches!(e, Error::Io(_) | Error::Format(_)), "{e}");
    assert!(lock(&laptop.host.code).as_ref().is_some_and(|c| c.left == CODE_TRIES), "чужая метка — даже не попытка");
    // Ответ чужого приложения назван понятно.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let t = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut head = [0u8; 8];
        s.read_exact(&mut head).unwrap();
        let mut body = vec![0u8; u16::from_be_bytes([head[6], head[7]]) as usize];
        s.read_exact(&mut body).unwrap();
        s.write_all(b"KRMLP\x01\x00\x02{}").unwrap();
    });
    let e = pair_join(&POLKA_LAN, local(port), "6WVGHEWK", &*phone.store, "phone", "Телефон", &|_, _| panic!()).unwrap_err();
    assert_eq!(e.to_string(), "это не «Полка»");
    t.join().unwrap();
    server.stop();
}

/// Более новая версия открытых сообщений — понятная просьба обновиться.
#[test]
fn newer_peer_asks_to_update() {
    let phone = side(&KARMAN_LAN, "phone", "Телефон");
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let t = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut head = [0u8; 8];
        s.read_exact(&mut head).unwrap();
        let mut body = vec![0u8; u16::from_be_bytes([head[6], head[7]]) as usize];
        s.read_exact(&mut body).unwrap();
        s.write_all(b"KRMLP\x02\x00\x02{}").unwrap();
    });
    let e = pair_join(&KARMAN_LAN, local(port), "6WVGHEWK", &*phone.store, "phone", "Телефон", &|_, _| panic!()).unwrap_err();
    assert_eq!(e.to_string(), "На другом устройстве более новая версия «Кармана»: обновите это устройство");
    t.join().unwrap();
}

fn pipe() -> (TcpStream, TcpStream) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let a = TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let (b, _) = l.accept().unwrap();
    (a, b)
}

#[test]
fn packets_out_of_order_or_replayed_do_not_decrypt() {
    type M = Msg<serde_json::Value>;
    let lan = &POLKA_LAN;
    let (a, b) = pipe();
    let mut tx = SessionWire::new(lan, a, [1u8; 32], [2u8; 32]);
    send(&mut tx, &M::Done { applied: 1 }).unwrap();
    send(&mut tx, &M::Done { applied: 2 }).unwrap();
    let mut raw = b.try_clone().unwrap();
    let mut frames = Vec::new();
    for _ in 0..2 {
        let mut len = [0u8; 4];
        raw.read_exact(&mut len).unwrap();
        let mut f = vec![0u8; u32::from_be_bytes(len) as usize];
        raw.read_exact(&mut f).unwrap();
        frames.push(f);
    }
    // Второй пакет первым — не расшифруется.
    let (mut c, d) = pipe();
    for f in [&frames[1], &frames[0]] {
        c.write_all(&(f.len() as u32).to_be_bytes()).unwrap();
        c.write_all(f).unwrap();
    }
    let mut rx = SessionWire::new(lan, d, [2u8; 32], [1u8; 32]);
    assert!(matches!(recv_max::<Value>(&mut rx, MAX_FRAME), Err(Error::Decrypt)), "пакет не на своём месте не расшифровывается");
    // Тот же первый пакет дважды: второй раз — уже не тот номер.
    let (mut e, f) = pipe();
    for _ in 0..2 {
        e.write_all(&(frames[0].len() as u32).to_be_bytes()).unwrap();
        e.write_all(&frames[0]).unwrap();
    }
    let mut rx = SessionWire::new(lan, f, [2u8; 32], [1u8; 32]);
    assert!(matches!(recv_max::<Value>(&mut rx, MAX_FRAME), Ok(M::Done { applied: 1 })));
    assert!(matches!(recv_max::<Value>(&mut rx, MAX_FRAME), Err(Error::Decrypt)), "повтор пакета не проходит");
    // Пакет «Полки» не расшифруется каналом «Кармана» с теми же ключами: метка — в AAD.
    let (mut g, h) = pipe();
    g.write_all(&(frames[0].len() as u32).to_be_bytes()).unwrap();
    g.write_all(&frames[0]).unwrap();
    let mut rx = SessionWire::new(&KARMAN_LAN, h, [2u8; 32], [1u8; 32]);
    assert!(matches!(recv_max::<Value>(&mut rx, MAX_FRAME), Err(Error::Decrypt)));
}

/// Объявленный огромный пакет без данных не занимает память: читается только то, что пришло.
#[test]
fn declared_big_frame_without_data_fails_fast() {
    let (mut a, b) = pipe();
    a.write_all(&((MAX_FRAME as u32 - 1).to_be_bytes())).unwrap();
    a.write_all(&[1u8; 100]).unwrap();
    drop(a);
    let mut rx = SessionWire::new(&POLKA_LAN, b, [2u8; 32], [1u8; 32]);
    assert!(matches!(recv_max::<Value>(&mut rx, MAX_FRAME), Err(Error::Io(_))), "обрыв — ошибка, а не 64 МБ памяти");
}

#[test]
fn server_drops_junk_quickly() {
    let laptop = side(&POLKA_LAN, "laptop", "Ноутбук");
    let server = Server::start(laptop.host.clone(), 0).unwrap();
    // Огромный открытый запрос — закрыто сразу, без чтения.
    let mut s = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    s.write_all(b"PLKLP\x01\xff\xff").unwrap();
    let t0 = Instant::now();
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = [0u8; 8];
    assert!(matches!(s.read(&mut buf), Ok(0) | Err(_)));
    assert!(t0.elapsed() < Duration::from_secs(2));
    // Запрос другого приложения — тоже отказ.
    let mut s = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    s.write_all(b"KRMLP\x01\x00\x02{}").unwrap();
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    assert!(matches!(s.read(&mut buf), Ok(0) | Err(_)));
    // Сразу шифрованный пакет (связь по QR-коду прежнего «Кармана») — отказ.
    let mut s = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    s.write_all(&[0, 0, 0, 40]).unwrap();
    s.write_all(b"KRM1").unwrap();
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    assert!(matches!(s.read(&mut buf), Ok(0) | Err(_)));
    assert!(lock(&laptop.keys).is_empty() && lock(&laptop.log).is_empty());
    server.stop();
}

#[test]
fn discovery_answers_only_with_code_or_to_peers() {
    for lan in [&POLKA_LAN, &KARMAN_LAN] {
        let laptop = side(lan, "laptop", "Ноутбук");
        let key = [9u8; 32];
        lock(&laptop.keys).insert("phone".into(), key);
        let resp = Responder::start(laptop.host.clone(), 0, lan.tcp_port).unwrap();
        let to = [local(resp.port)];
        let wait = Duration::from_millis(400);
        // Чужому без кода — тишина.
        assert!(discover(lan, &to, "stranger", &[], wait).unwrap().is_empty(), "чужому в сети не отвечаем");
        // Связанному — ответ с адресом, подписанный ключом пары.
        let f = discover(lan, &to, "phone", &[("laptop".into(), key)], wait).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].device.as_str(), f[0].peer, f[0].pairing, f[0].addr), ("laptop", true, false, local(lan.tcp_port)));
        assert_eq!((f[0].name.as_str(), f[0].app.as_str()), ("", lan.profile.version), "имя связанным не нужно и не отдаётся");
        // Подпись чужим ключом — тишина.
        assert!(discover(lan, &to, "phone", &[("laptop".into(), [8u8; 32])], wait).unwrap().is_empty());
        // Показан код — отвечаем всем: имя и порт.
        *lock(&laptop.host.code) = Some(CodeState::new("6WVGHEWK".into()));
        let f = discover(lan, &to, "stranger", &[], wait).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].name.as_str(), f[0].pairing, f[0].peer), ("Ноутбук", true, false));
        // Поиск другого приложения — тишина и с кодом.
        let other = if std::ptr::eq(lan, &POLKA_LAN) { &KARMAN_LAN } else { &POLKA_LAN };
        assert!(discover(other, &to, "stranger", &[], wait).unwrap().is_empty());
        // Код отменён — снова тишина.
        lock(&laptop.host.code).as_mut().unwrap().left = 0;
        assert!(discover(lan, &to, "stranger", &[], wait).unwrap().is_empty());
        resp.stop();
    }
}

/// Подставной ответ на поиск без ключа пары не принимается.
#[test]
fn forged_answer_is_ignored() {
    let fake = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = fake.local_addr().unwrap().port();
    let t = std::thread::spawn(move || {
        let mut buf = [0u8; 1500];
        fake.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        for _ in 0..2 {
            let Ok((_, src)) = fake.recv_from(&mut buf) else { return };
            let a = json!({ "t": "peer", "d": "laptop", "p": 47510, "m": b64(&[0u8; 32]) });
            fake.send_to(&POLKA_LAN.datagram(&POLKA_LAN.answer_magic, &a), src).unwrap();
        }
    });
    let f = discover(&POLKA_LAN, &[local(port)], "phone", &[("laptop".into(), [9u8; 32])], Duration::from_millis(500)).unwrap();
    assert!(f.is_empty(), "{f:?}");
    t.join().unwrap();
}

#[test]
fn only_home_network_addresses() {
    for ok in ["192.168.1.20", "10.0.0.5", "172.20.10.2", "192.168.43.1", "100.101.1.2", "169.254.3.4", "127.0.0.1", "fe80::1", "fd12:3456::1", "::1"] {
        assert!(is_local_address(ok), "{ok}");
    }
    for bad in ["8.8.8.8", "93.184.216.34", "172.32.0.1", "100.128.0.1", "evil.example.com", "localhost", "2001:db8::1", ""] {
        assert!(!is_local_address(bad), "{bad}");
    }
}

#[test]
fn codes_are_random_readable_and_forgiving() {
    let a = new_code().unwrap();
    let b = new_code().unwrap();
    assert_eq!(a.len(), CODE_LEN);
    assert_ne!(a, b);
    assert!(a.bytes().all(|c| CODE_ALPHABET.contains(&c)), "{a}");
    assert_eq!(show_code("6WVGHEWK"), "6WVG-HEWK");
    assert_eq!(normalize_code(" 6wvg-hewk ").unwrap(), "6WVGHEWK");
    // С русской раскладки: «Н», «Е», «В», «К», «Р», «С», «Т», «М», «Х», «А» — как латинские.
    assert_eq!(normalize_code("НЕВК–РСТМ").unwrap(), "HEBKPCTM");
    assert_eq!(normalize_code("ха3у ксам").unwrap(), "XA3YKCAM");
    assert!(normalize_code("6WVG-HEW").unwrap_err().to_string().contains("восемь знаков"));
    assert!(normalize_code("6WVG-HEW0").unwrap_err().to_string().contains("«0»"), "нуля в коде не бывает");
    assert!(normalize_code("6WVG-HEWЖ").unwrap_err().to_string().contains("«Ж»"));
}

#[test]
fn invite_qr_roundtrip() {
    let inv = Invite { device: "a1b2".into(), name: "Ноутбук".into(), hosts: vec!["192.168.1.20".into(), "8.8.8.8".into()], port: 47_510, code: "6WVGHEWK".into() };
    assert!(inv.to_uri(&POLKA_LAN).starts_with("polka://pair?v=1&d=a1b2&"));
    assert!(inv.to_uri(&KARMAN_LAN).starts_with("karman://pair?v=2&d=a1b2&"));
    let back = Invite::parse(&POLKA_LAN, &inv.to_uri(&POLKA_LAN)).unwrap();
    assert_eq!(back, Invite { hosts: vec!["192.168.1.20".into()], ..inv.clone() }, "адрес из интернета отброшен");
    let only_internet = Invite { hosts: vec!["8.8.8.8".into()], ..inv.clone() };
    assert!(Invite::parse(&POLKA_LAN, &only_internet.to_uri(&POLKA_LAN)).unwrap_err().to_string().contains("не из домашней сети"));
    assert!(Invite::parse(&POLKA_LAN, &inv.to_uri(&POLKA_LAN).replace("c=6WVGHEWK", "c=123")).is_err());
    assert!(Invite::parse(&POLKA_LAN, "polka://pair?v=2&d=x").unwrap_err().to_string().contains("обновите «Полку»"));
    assert_eq!(Invite::parse(&POLKA_LAN, &inv.to_uri(&KARMAN_LAN)).unwrap_err().to_string(), "это не QR-код «Полки» для связи");
    assert!(Invite::parse(&KARMAN_LAN, "karman://pair?v=3&d=x").unwrap_err().to_string().contains("обновите «Карман»"));
    assert!(Invite::parse(&KARMAN_LAN, "https://example.com").is_err());
}
