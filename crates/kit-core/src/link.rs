//! Связь коннектора с запущенным приложением на компьютере.
//!
//! Приложение Claude запускает `<программа> --mcp` (расширение `.mcpb`) и говорит с ним по протоколу MCP
//! через stdio ([`relay`]). Посредник сам отвечает на `initialize` и `tools/list`, а вызовы инструментов
//! передаёт запущенному приложению по 127.0.0.1 — запросы из сети приложение не принимает ([`listen`]).
//!
//! Ключ связи новый при каждом запуске приложения и лежит в его папке данных (файл [`LINK_FILE`]); по сети
//! он не ходит: стороны доказывают друг другу, что знают его, — MAC от одноразовых чисел
//! ([`mcp::link_proof`](crate::mcp::link_proof)). Программа, занявшая порт закрытого приложения, не получит
//! ни ключа, ни запроса. Если приложение закрыто, посредник на Windows его запускает.
//!
//! Обмен (каждая строка — JSON):
//! 1. посредник → приложение: `{"hello": <число посредника>}`;
//! 2. приложение → посредник: `{"proof": MAC(ключ, <имя>|<число посредника>), "nonce": <число приложения>}`;
//! 3. посредник → приложение: `{"auth": MAC(ключ, bridge|<число приложения>), "tool": …, "args": …}`;
//! 4. приложение → посредник: `{"ok": …}` или `{"err": "…"}`.
//!
//! Так «Полка» и «Карман» говорили и до общего модуля: посредник прежней версии и новое приложение
//! (и наоборот) понимают друг друга.

use crate::mcp::{self, Extension, Server, Tool};
use crate::AppNames;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Где запущенное приложение ждёт посредника: порт и ключ. Лежит в папке данных приложения.
pub const LINK_FILE: &str = "claude.json";
/// Запрос посредника — одна строка JSON, не больше.
const MAX_REQUEST: u64 = 1 << 20;
/// Ответ приложения — не больше.
const MAX_REPLY: u64 = 64 << 20;
/// Сколько приложение ждёт посредника на каждом шаге обмена.
const SERVE_TIMEOUT: Duration = Duration::from_secs(10);
/// Сколько посредник ждёт ответа: отчёт с нарядами или поиск обложки в каталогах — дольше обычного запроса.
const REPLY_TIMEOUT: Duration = Duration::from_secs(120);
/// Сколько посредник ждёт, пока запущенное им приложение откроется.
const LAUNCH_WAIT: Duration = Duration::from_secs(40);

/// Коннектор приложения: всё, чем приложения различаются в связи.
#[derive(Clone, Copy, Debug)]
pub struct Connector {
    /// Имя (оно же роль приложения в доказательствах ключа) и строка ключа связи.
    pub server: Server,
    /// Переменная окружения с папкой данных (`dir_env`) — её задаёт расширение.
    pub extension: Extension,
    /// Название для сообщений.
    pub names: AppNames,
    /// Идентификатор приложения (`identifier` из `tauri.conf.json`) — имя папки данных.
    pub identifier: &'static str,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Link {
    port: u16,
    token: String,
    pid: u32,
    /// Версия приложения, записавшего файл.
    app: String,
}

/// Запрос посредника — после того как приложение доказало, что знает ключ связи.
#[derive(Serialize, Deserialize)]
struct Request {
    /// Доказательство посредника: MAC от одноразового числа приложения.
    auth: String,
    tool: String,
    #[serde(default)]
    args: Value,
}

#[derive(Serialize, Deserialize, Default)]
struct Reply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ok: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    err: Option<String>,
}

/// 32 случайных байта шестнадцатеричными знаками: одноразовое число или ключ связи.
fn random_hex() -> Option<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).ok()?;
    Some(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn valid_nonce(n: &str) -> bool {
    n.len() == 64 && n.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Сравнение без тайминговых подсказок: всегда до конца.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ——— Приложение: приём запросов посредника ———

/// Посредник прислал своё одноразовое число — отвечаем доказательством и своим числом; дальше принимаем
/// запрос, только если посредник доказал, что знает ключ. `None` — связь оборвалась.
fn accept(c: &Connector, stream: &TcpStream, token: &str) -> Option<Result<Request, String>> {
    let mut reader = BufReader::new(stream.take(MAX_REQUEST));
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let hello: Value = match serde_json::from_str(&line) {
        Ok(v) => v,
        Err(_) => return Some(Err("неверный запрос".into())),
    };
    let Some(theirs) = hello["hello"].as_str().filter(|n| valid_nonce(n)) else {
        // Посредник совсем старой версии присылал ключ открытым текстом.
        return Some(Err(if hello.get("token").is_some() { outdated(&c.names) } else { "неверный запрос".into() }));
    };
    let Some(mine) = random_hex() else { return Some(Err("нет случайных чисел".into())) };
    let mut out = stream;
    writeln!(out, "{}", json!({ "proof": mcp::link_proof(&c.server, token, c.server.name, theirs), "nonce": mine })).ok()?;
    line.clear();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    Some(match serde_json::from_str::<Request>(&line) {
        Ok(r) if same(&r.auth, &mcp::link_proof(&c.server, token, "bridge", &mine)) => Ok(r),
        Ok(_) => Err("неверный ключ".into()),
        Err(_) => Err("неверный запрос".into()),
    })
}

fn serve(c: &Connector, stream: TcpStream, token: &str, run: &(dyn Fn(&str, &Value) -> Result<Value, String> + Send + Sync)) {
    let _ = stream.set_read_timeout(Some(SERVE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SERVE_TIMEOUT));
    let reply = match accept(c, &stream, token) {
        None => return,
        Some(Ok(r)) => match run(&r.tool, &r.args) {
            Ok(v) => Reply { ok: Some(v), err: None },
            Err(e) => Reply { ok: None, err: Some(e) },
        },
        Some(Err(e)) => Reply { ok: None, err: Some(e) },
    };
    let mut out = &stream;
    let _ = writeln!(out, "{}", serde_json::to_string(&reply).unwrap_or_default());
}

fn write_link(dir: &Path, link: &Link) -> std::io::Result<()> {
    let path = dir.join(LINK_FILE);
    let tmp = dir.join(format!("{LINK_FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec(link)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(tmp, path)
}

/// Запущенное приложение: ждём посредника на 127.0.0.1 со свежим ключом и записываем в папку данных,
/// где нас искать. Каждый запрос — в своём потоке; `run(инструмент, аргументы)` выполняет инструмент:
/// `Ok` — данные, `Err` — понятная человеку ошибка. Возвращает порт.
///
/// Вызывать после того, как приложение готово отвечать: первый же запрос может прийти сразу.
pub fn listen(c: &Connector, data_dir: &Path, version: &str, run: impl Fn(&str, &Value) -> Result<Value, String> + Send + Sync + 'static) -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let token = random_hex().ok_or_else(|| std::io::Error::other("нет случайных чисел"))?;
    write_link(data_dir, &Link { port, token: token.clone(), pid: std::process::id(), app: version.into() })?;
    let c = *c;
    let run = std::sync::Arc::new(run);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (token, run) = (token.clone(), run.clone());
            std::thread::spawn(move || serve(&c, stream, &token, &*run));
        }
    });
    Ok(port)
}

// ——— Посредник: `<программа> --mcp` ———

/// Папка данных приложения — та же, что у запущенного приложения (Tauri: `app_data_dir`). Переменная
/// окружения из расширения (`KARMAN_DIR`, `POLKA_DIR`) важнее.
pub fn data_dir(c: &Connector) -> PathBuf {
    if let Some(d) = std::env::var_os(c.extension.dir_env).filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()).map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.unwrap_or_default().join(c.identifier)
}

#[derive(Debug, PartialEq)]
enum CallError {
    /// Приложение не запущено (или ещё не успело открыться), или на порту не оно.
    NotRunning,
    Failed(String),
}

fn call_app(c: &Connector, dir: &Path, tool: &str, args: &Value) -> Result<Value, CallError> {
    let link: Link = std::fs::read(dir.join(LINK_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).ok_or(CallError::NotRunning)?;
    let addr = SocketAddr::from(([127, 0, 0, 1], link.port));
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|_| CallError::NotRunning)?;
    let _ = stream.set_read_timeout(Some(REPLY_TIMEOUT));
    talk(c, &stream, &link.token, tool, args)
}

/// Сначала приложение доказывает, что знает ключ связи, потом уходит запрос с доказательством посредника.
/// Если на порту не приложение (порт заняла другая программа), запрос не уходит.
fn talk(c: &Connector, stream: &TcpStream, token: &str, tool: &str, args: &Value) -> Result<Value, CallError> {
    let n = &c.names;
    let mine = random_hex().ok_or_else(|| CallError::Failed("нет случайных чисел".into()))?;
    let mut out = stream;
    writeln!(out, "{}", json!({ "hello": mine })).map_err(|_| CallError::NotRunning)?;
    let mut reader = BufReader::new(stream.take(MAX_REPLY));
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|_| CallError::NotRunning)?;
    let challenge: Value = serde_json::from_str(&line).map_err(|_| CallError::NotRunning)?;
    if let Some(e) = challenge["err"].as_str() {
        // Приложение не приняло приветствие — например, оно прежней версии и не знает о проверке.
        return Err(CallError::Failed(format!("«{}» не {} запрос ({e}) — перезапустите приложение Claude и «{}»", n.name, n.g("принял", "приняла"), n.name_acc)));
    }
    let proof = challenge["proof"].as_str().unwrap_or_default();
    let Some(theirs) = challenge["nonce"].as_str().filter(|x| valid_nonce(x)) else { return Err(CallError::NotRunning) };
    if proof.is_empty() || !same(proof, &mcp::link_proof(&c.server, token, c.server.name, &mine)) {
        // На порту не приложение: ни ключа, ни запроса ему не отдаём.
        return Err(CallError::NotRunning);
    }
    let req = Request { auth: mcp::link_proof(&c.server, token, "bridge", theirs), tool: tool.to_string(), args: args.clone() };
    writeln!(out, "{}", serde_json::to_string(&req).map_err(|e| CallError::Failed(e.to_string()))?).map_err(|_| CallError::NotRunning)?;
    line.clear();
    reader.read_line(&mut line).map_err(|e| CallError::Failed(format!("«{}» не {}: {e}", n.name, n.g("ответил", "ответила"))))?;
    let reply: Reply = serde_json::from_str(&line).map_err(|_| CallError::Failed(format!("«{}» {} непонятно — обновите {}", n.name, n.g("ответил", "ответила"), n.g("его", "её"))))?;
    match (reply.ok, reply.err) {
        (Some(v), _) => Ok(v),
        (None, Some(e)) => Err(CallError::Failed(e)),
        _ => Err(CallError::Failed(format!("пустой ответ «{}»", n.name_gen))),
    }
}

/// Посредник совсем старой версии: ключ открытым текстом.
fn outdated(n: &AppNames) -> String {
    format!("«{}» {} — перезапустите приложение Claude, чтобы обновился и коннектор", n.name, n.g("обновился", "обновилась"))
}

fn not_running(n: &AppNames) -> String {
    format!("«{}» не {}: откройте {} на этом компьютере и повторите запрос.", n.name, n.g("запущен", "запущена"), n.g("его", "её"))
}

fn not_opened(n: &AppNames) -> String {
    format!("«{}» не {} за 40 секунд: откройте {} вручную и повторите запрос.", n.name, n.g("открылся", "открылась"), n.g("его", "её"))
}

/// Запустить приложение отдельно от приложения Claude (чтобы оно не закрылось вместе с ним).
fn launch() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let mut cmd = std::process::Command::new(&exe);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_BREAKAWAY_FROM_JOB | CREATE_NEW_PROCESS_GROUP);
        cmd.spawn().is_ok() || {
            // Если выйти из задания приложения Claude нельзя — обычный запуск.
            let mut plain = std::process::Command::new(&exe);
            plain.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            plain.creation_flags(CREATE_NEW_PROCESS_GROUP);
            plain.spawn().is_ok()
        }
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
        false
    }
}

/// Программа запущена посредником: `<программа> --mcp`. Только тогда она запускает закрытое приложение —
/// сама собой, то есть приложением. Из проверок и любого другого места запуска нет.
fn is_relay() -> bool {
    std::env::args().nth(1).as_deref() == Some("--mcp")
}

/// Передать вызов инструмента запущенному приложению. Если оно закрыто, а это посредник на Windows, —
/// запустить приложение (там второй запуск лишь открывает уже работающее) и подождать до 40 секунд.
pub fn forward(c: &Connector, dir: &Path, tool: &str, args: &Value) -> Result<Value, String> {
    match call_app(c, dir, tool, args) {
        Ok(v) => return Ok(v),
        Err(CallError::Failed(e)) => return Err(e),
        Err(CallError::NotRunning) => {}
    }
    if !cfg!(windows) || !is_relay() || !launch() {
        return Err(not_running(&c.names));
    }
    let started = Instant::now();
    while started.elapsed() < LAUNCH_WAIT {
        std::thread::sleep(Duration::from_millis(400));
        match call_app(c, dir, tool, args) {
            Ok(v) => return Ok(v),
            Err(CallError::Failed(e)) => return Err(e),
            Err(CallError::NotRunning) => {}
        }
    }
    Err(not_opened(&c.names))
}

/// Посредник на заданных потоках: строка запроса MCP → строка ответа; вызовы инструментов — запущенному
/// приложению в `dir`. Код выхода — 0, когда входной поток закрыт.
pub fn relay_io(c: &Connector, dir: &Path, tools: &[Tool], version: &str, input: impl BufRead, mut output: impl Write) -> i32 {
    for line in input.lines() {
        let Ok(line) = line else { return 1 };
        if line.trim().is_empty() {
            continue;
        }
        let reply = mcp::handle(&c.server, tools, &line, version, &mut |tool, args| forward(c, dir, tool, args));
        if let Some(r) = reply {
            if writeln!(output, "{r}").and_then(|_| output.flush()).is_err() {
                return 1;
            }
        }
    }
    0
}

/// `<программа> --mcp`: MCP по stdio для приложения Claude. Код выхода — 0, когда приложение Claude закрыло поток.
pub fn relay(c: &Connector, tools: &[Tool], version: &str) -> i32 {
    let dir = data_dir(c);
    let stdin = std::io::stdin();
    relay_io(c, &dir, tools, version, stdin.lock(), std::io::stdout().lock())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const SERVER: Server = Server { name: "proba", title: "Проба", instructions: "Подсказка.", link_personal: b"proba-link" };
    const EXT: Extension = Extension { display_name: "Проба", description: "", long_description: "", author: "Проба", keywords: &[], dir_env: "KIT_TEST_PROBA_DIR" };
    const C: Connector = Connector { server: SERVER, extension: EXT, names: AppNames { id: "proba", name: "Проба", name_acc: "Пробу", name_gen: "Пробы", feminine: true }, identifier: "io.github.primusfixer.proba" };

    fn tools() -> Vec<Tool> {
        vec![Tool { name: "summary", title: "Сводка", description: "", input_schema: json!({ "type": "object" }), write: false, destructive: false, idempotent: true }]
    }

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("kit-link-{}", random_hex().unwrap()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn app(dir: &Path) -> u16 {
        listen(&C, dir, "1.2.3", |tool, args| match tool {
            "summary" => Ok(json!({ "books": 24, "args": args })),
            _ => Err("«Проба» заблокирована".into()),
        })
        .unwrap()
    }

    #[test]
    fn relay_calls_the_running_app() {
        let dir = tmp();
        let port = app(&dir);
        let link: Link = serde_json::from_slice(&std::fs::read(dir.join(LINK_FILE)).unwrap()).unwrap();
        assert_eq!((link.port, link.app.as_str(), link.pid), (port, "1.2.3", std::process::id()));
        assert!(valid_nonce(&link.token), "ключ связи — 32 случайных байта");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join(LINK_FILE)).unwrap().permissions().mode() & 0o777, 0o600, "файл с ключом читает только владелец");
        }
        assert_eq!(forward(&C, &dir, "summary", &json!({ "a": 1 })), Ok(json!({ "books": 24, "args": { "a": 1 } })));
        assert_eq!(forward(&C, &dir, "shelf", &json!({})), Err("«Проба» заблокирована".into()));
    }

    #[test]
    fn relay_over_stdio_answers_mcp() {
        let dir = tmp();
        app(&dir);
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            "\n\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"summary","arguments":{}}}"#,
            "\n"
        );
        let mut out = Vec::new();
        assert_eq!(relay_io(&C, &dir, &tools(), "1.2.3", input.as_bytes(), &mut out), 0);
        let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2, "на уведомление и пустую строку не отвечают");
        assert_eq!(lines[0]["result"]["serverInfo"]["name"], "proba");
        assert_eq!(lines[1]["result"]["isError"], false);
        assert!(lines[1]["result"]["content"][0]["text"].as_str().unwrap().contains("24"));
    }

    #[test]
    fn closed_app_and_wrong_key_read_as_not_running() {
        let dir = tmp();
        // Приложение не запущено: файла связи нет. Проверка — не посредник (`--mcp`), поэтому и на Windows
        // она ничего не запускает, а сразу говорит, что приложение закрыто.
        assert!(!is_relay());
        assert_eq!(forward(&C, &dir, "summary", &json!({})), Err("«Проба» не запущена: откройте её на этом компьютере и повторите запрос.".into()));
        // Файл связи с чужим ключом: приложение не докажет ключ — посредник считает, что на порту не оно.
        let port = app(&dir);
        write_link(&dir, &Link { port, token: "cd".repeat(32), pid: 1, app: "1.2.3".into() }).unwrap();
        assert_eq!(call_app(&C, &dir, "summary", &json!({})), Err(CallError::NotRunning));
    }

    /// Порт заняла другая программа: она не знает ключа — запрос с аргументами к ней не уходит.
    #[test]
    fn impostor_on_the_port_gets_no_request() {
        let dir = tmp();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        write_link(&dir, &Link { port, token: "ab".repeat(32), pid: 1, app: "1.2.3".into() }).unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let s = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut r = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                s.lock().unwrap().push(line.clone());
                let mut w = &stream;
                let _ = writeln!(w, "{}", json!({ "proof": "00".repeat(32), "nonce": "11".repeat(32) }));
                line.clear();
                let _ = r.read_line(&mut line);
                s.lock().unwrap().push(line);
            }
        });
        assert_eq!(call_app(&C, &dir, "summary", &json!({ "secret": "аргументы" })), Err(CallError::NotRunning));
        std::thread::sleep(Duration::from_millis(100));
        let seen = seen.lock().unwrap().join("");
        assert!(seen.contains("hello"), "приветствие дошло");
        assert!(!seen.contains("tool") && !seen.contains("аргументы") && !seen.contains(&"ab".repeat(32)), "ни запроса, ни ключа: {seen}");
    }

    /// Чужой посредник без ключа: приложение не выполняет запрос.
    #[test]
    fn app_rejects_a_relay_without_the_key() {
        let dir = tmp();
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = runs.clone();
        let port = listen(&C, &dir, "1.2.3", move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(json!(null))
        })
        .unwrap();
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut w = &stream;
        let mut r = BufReader::new(&stream);
        writeln!(w, "{}", json!({ "hello": "01".repeat(32) })).unwrap();
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let challenge: Value = serde_json::from_str(&line).unwrap();
        assert!(valid_nonce(challenge["nonce"].as_str().unwrap()));
        writeln!(w, "{}", json!({ "auth": "00".repeat(32), "tool": "summary", "args": {} })).unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), json!({ "err": "неверный ключ" }));
        assert_eq!(runs.load(Ordering::SeqCst), 0);

        // Совсем старый посредник присылал ключ открытым текстом — просим перезапустить приложение Claude.
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut w = &stream;
        writeln!(w, "{}", json!({ "token": "ab".repeat(32), "tool": "summary" })).unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["err"], "«Проба» обновилась — перезапустите приложение Claude, чтобы обновился и коннектор");
    }

    /// Тексты для обоих родов — как были в «Полке» и «Кармане».
    #[test]
    fn messages_follow_the_gender_of_the_name() {
        let karman = AppNames { id: "karman", name: "Карман", name_acc: "Карман", name_gen: "Кармана", feminine: false };
        let polka = AppNames { id: "polka", name: "Полка", name_acc: "Полку", name_gen: "Полки", feminine: true };
        assert_eq!(not_running(&karman), "«Карман» не запущен: откройте его на этом компьютере и повторите запрос.");
        assert_eq!(not_running(&polka), "«Полка» не запущена: откройте её на этом компьютере и повторите запрос.");
        assert_eq!(not_opened(&karman), "«Карман» не открылся за 40 секунд: откройте его вручную и повторите запрос.");
        assert_eq!(not_opened(&polka), "«Полка» не открылась за 40 секунд: откройте её вручную и повторите запрос.");
        assert_eq!(outdated(&karman), "«Карман» обновился — перезапустите приложение Claude, чтобы обновился и коннектор");
    }

    // ——— Совместимость с «Полкой» и «Карманом» до общего модуля ———
    //
    // Ниже — обмен в том виде, как он был написан в приложениях до kit (строки протокола и доказательства
    // вписаны прямо в код). Посредник прежней версии должен говорить с новым приложением, и наоборот:
    // после обновления приложение Claude может держать запущенным старый посредник.

    const KARMAN: Server = Server { name: "karman", title: "Карман", instructions: "", link_personal: b"karman-link" };

    /// Доказательство ключа, как его считал «Карман» до kit.
    fn proof_before(token: &str, role: &str, nonce: &str) -> String {
        use blake2::digest::consts::U32;
        use blake2::digest::Mac;
        let mut mac = blake2::Blake2bMac::<U32>::new_with_salt_and_personal(token.as_bytes(), &[], b"karman-link").unwrap();
        mac.update(role.as_bytes());
        mac.update(b"|");
        mac.update(nonce.as_bytes());
        mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Посредник «Кармана» до kit (`talk`), сокращённо: те же строки по тем же шагам.
    fn relay_before(port: u16, token: &str, tool: &str, args: &Value) -> Result<Value, String> {
        let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
        let mine = "5a".repeat(32);
        let mut out = &stream;
        writeln!(out, "{}", json!({ "hello": mine })).unwrap();
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let ch: Value = serde_json::from_str(&line).unwrap();
        let theirs = ch["nonce"].as_str().unwrap();
        if ch["proof"].as_str() != Some(proof_before(token, "karman", &mine).as_str()) {
            return Err("на порту не «Карман»".into());
        }
        writeln!(out, "{}", json!({ "auth": proof_before(token, "bridge", theirs), "tool": tool, "args": args })).unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        match (reply.get("ok"), reply.get("err")) {
            (Some(v), _) => Ok(v.clone()),
            (None, Some(e)) => Err(e.as_str().unwrap().to_string()),
            _ => Err("пустой ответ".into()),
        }
    }

    /// Приложение «Карман» до kit (`accept` и `serve`), сокращённо: отвечает `ok` с именем инструмента.
    fn app_before(dir: &Path) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let token = "7e".repeat(32);
        std::fs::write(dir.join("claude.json"), json!({ "port": port, "token": token, "pid": 1, "app": "0.15.1" }).to_string()).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let hello: Value = serde_json::from_str(&line).unwrap();
                let mine = "c3".repeat(32);
                let mut out = &stream;
                writeln!(out, "{}", json!({ "proof": proof_before(&token, "karman", hello["hello"].as_str().unwrap()), "nonce": mine })).unwrap();
                line.clear();
                reader.read_line(&mut line).unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let reply = if req["auth"].as_str() == Some(proof_before(&token, "bridge", &mine).as_str()) {
                    json!({ "ok": { "tool": req["tool"], "args": req["args"] } })
                } else {
                    json!({ "err": "неверный ключ" })
                };
                writeln!(out, "{reply}").unwrap();
            }
        });
        port
    }

    #[test]
    fn relay_before_kit_talks_to_the_new_app() {
        let dir = tmp();
        let c = Connector { server: KARMAN, ..C };
        let port = listen(&c, &dir, "0.15.5", |tool, args| Ok(json!({ "tool": tool, "args": args }))).unwrap();
        let link: Link = serde_json::from_slice(&std::fs::read(dir.join("claude.json")).unwrap()).unwrap();
        assert_eq!(relay_before(port, &link.token, "summary", &json!({ "x": 1 })), Ok(json!({ "tool": "summary", "args": { "x": 1 } })));
    }

    #[test]
    fn new_relay_talks_to_the_app_before_kit() {
        let dir = tmp();
        app_before(&dir);
        let c = Connector { server: KARMAN, ..C };
        assert_eq!(forward(&c, &dir, "accounts", &json!({ "y": 2 })), Ok(json!({ "tool": "accounts", "args": { "y": 2 } })));
    }
}
