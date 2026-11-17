//! Коннектор приложения к приложению Claude: протокол MCP (Model Context Protocol) — JSON-RPC 2.0
//! построчно через stdio.
//!
//! Здесь разбор и ответы (`initialize`, `ping`, `tools/list`, `tools/call`), доказательство ключа связи
//! и файл расширения `.mcpb`. Сам вызов инструмента — снаружи: посредник `<программа> --mcp` передаёт его
//! запущенному приложению. Чем приложения различаются, описывают [`Server`], [`Extension`] и список [`Tool`].

use serde_json::{json, Value};
use std::io::Write;

/// Версии протокола, которые понимает коннектор, — новая первой.
pub const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Коннектор приложения: имя для Claude, подсказка и строка ключа связи.
#[derive(Clone, Copy, Debug)]
pub struct Server {
    /// Латиницей: `polka`.
    pub name: &'static str,
    /// Как его видит человек: «Полка».
    pub title: &'static str,
    /// Подсказка для Claude: что это за данные и как с ними обращаться.
    pub instructions: &'static str,
    /// Личная строка BLAKE2b для доказательства ключа связи (`polka-link`). **Менять нельзя:** посредник
    /// из прежней версии и запущенное приложение перестанут узнавать друг друга.
    pub link_personal: &'static [u8],
}

/// Инструмент для Claude.
#[derive(Clone, Debug)]
pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    /// Меняет данные: работает, только если запись разрешена в настройках приложения.
    pub write: bool,
    /// Удаляет (подсказка Claude: переспросить человека).
    pub destructive: bool,
    /// Повтор с теми же данными ничего не меняет.
    pub idempotent: bool,
}

fn ok(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn fail(id: &Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

/// Доказательство ключа связи посредника (`<программа> --mcp`) и запущенного приложения: MAC от одноразового
/// числа другой стороны. Сам ключ по сети не ходит, и программа, занявшая порт, ничего не узнает.
pub fn link_proof(server: &Server, token: &str, role: &str, nonce: &str) -> String {
    use blake2::digest::consts::U32;
    use blake2::digest::Mac;
    // Ключ BLAKE2b — не длиннее 64 байт (ключ связи — 64 шестнадцатеричных знака).
    if token.is_empty() || token.len() > 64 {
        return String::new();
    }
    let Ok(mut mac) = blake2::Blake2bMac::<U32>::new_with_salt_and_personal(token.as_bytes(), &[], server.link_personal) else { return String::new() };
    mac.update(role.as_bytes());
    mac.update(b"|");
    mac.update(nonce.as_bytes());
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// Описание инструментов для `tools/list`.
pub fn tool_list(tools: &[Tool]) -> Value {
    let tools: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "title": t.title,
                "description": t.description,
                "inputSchema": t.input_schema,
                "annotations": {
                    "title": t.title,
                    "readOnlyHint": !t.write,
                    "destructiveHint": t.destructive,
                    "idempotentHint": !t.write || t.idempotent,
                    "openWorldHint": false
                }
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// Ответ на одну строку запроса; `None` — для уведомлений, на них не отвечают.
/// `call(имя, аргументы)` выполняет инструмент: `Ok` — данные, `Err` — понятная человеку ошибка.
pub fn handle(server: &Server, tools: &[Tool], line: &str, version: &str, call: &mut dyn FnMut(&str, &Value) -> Result<Value, String>) -> Option<String> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Some(fail(&Value::Null, -32700, "Parse error")),
    };
    if !msg.is_object() {
        return Some(fail(&Value::Null, -32600, "Invalid Request"));
    }
    let method = msg.get("method").and_then(|m| m.as_str());
    let id = msg.get("id").cloned();
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
    // Ответ клиента на наш запрос (мы их не шлём) или уведомление — молча.
    let (Some(method), Some(id)) = (method, id) else { return None };
    Some(match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
            let version_used = if VERSIONS.contains(&asked) { asked } else { VERSIONS[0] };
            ok(
                &id,
                json!({
                    "protocolVersion": version_used,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": server.name, "title": server.title, "version": version },
                    "instructions": server.instructions
                }),
            )
        }
        "ping" => ok(&id, json!({})),
        "tools/list" => ok(&id, tool_list(tools)),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if !tools.iter().any(|t| t.name == name) {
                return Some(fail(&id, -32602, &format!("Unknown tool: {name}")));
            }
            let args = match params.get("arguments") {
                None | Some(Value::Null) => json!({}),
                Some(a) if a.is_object() => a.clone(),
                Some(_) => return Some(fail(&id, -32602, "arguments must be an object")),
            };
            match call(name, &args) {
                Ok(v) => ok(&id, json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }], "isError": false })),
                Err(e) => ok(&id, json!({ "content": [{ "type": "text", "text": e }], "isError": true })),
            }
        }
        "resources/list" => ok(&id, json!({ "resources": [] })),
        "resources/templates/list" => ok(&id, json!({ "resourceTemplates": [] })),
        "prompts/list" => ok(&id, json!({ "prompts": [] })),
        _ => fail(&id, -32601, &format!("Method not found: {method}")),
    })
}

/// Расширение для приложения Claude (`.mcpb`): как оно называется и что о себе рассказывает.
#[derive(Clone, Copy, Debug)]
pub struct Extension {
    pub display_name: &'static str,
    pub description: &'static str,
    pub long_description: &'static str,
    pub author: &'static str,
    pub keywords: &'static [&'static str],
    /// Переменная окружения с папкой данных приложения, которую получает посредник: `POLKA_DIR`.
    pub dir_env: &'static str,
}

/// Описание расширения (`manifest.json` внутри `.mcpb`).
/// `command` — полный путь к программе, `dir` — папка данных, `platform` — win32, darwin или linux.
pub fn manifest(server: &Server, ext: &Extension, tools: &[Tool], version: &str, command: &str, dir: &str, platform: &str) -> Value {
    // Путь может быть виндовым и там, где собирают проверки, — режем по обоим разделителям.
    let entry = command.rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or(server.name);
    let mut env = serde_json::Map::new();
    env.insert(ext.dir_env.to_string(), json!(dir));
    json!({
        "manifest_version": "0.3",
        "name": server.name,
        "display_name": ext.display_name,
        "version": version,
        "description": ext.description,
        "long_description": ext.long_description,
        "author": { "name": ext.author },
        "icon": "icon.png",
        "server": {
            "type": "binary",
            "entry_point": entry,
            "mcp_config": { "command": command, "args": ["--mcp"], "env": env }
        },
        "tools": tools.iter().map(|t| json!({ "name": t.name, "description": t.title })).collect::<Vec<_>>(),
        "compatibility": { "platforms": [platform] },
        "keywords": ext.keywords,
        "license": "Proprietary"
    })
}

/// Файл расширения `.mcpb`: архив ZIP с `manifest.json` и значком.
pub fn bundle(manifest: &Value, icon_png: &[u8]) -> Result<Vec<u8>, String> {
    let err = |e: zip::result::ZipError| format!("расширение не собралось: {e}");
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zw.start_file("manifest.json", opts).map_err(err)?;
    zw.write_all(&serde_json::to_vec_pretty(manifest).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    // PNG уже сжат — кладём как есть.
    zw.start_file("icon.png", opts.compression_method(zip::CompressionMethod::Stored)).map_err(err)?;
    zw.write_all(icon_png).map_err(|e| e.to_string())?;
    Ok(zw.finish().map_err(err)?.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: Server = Server { name: "proba", title: "Проба", instructions: "Подсказка для Claude.", link_personal: b"proba-link" };
    const EXT: Extension = Extension {
        display_name: "Проба",
        description: "Описание.",
        long_description: "Подробное описание.",
        author: "Проба",
        keywords: &["test"],
        dir_env: "PROBA_DIR",
    };

    fn tools() -> Vec<Tool> {
        let t = |name, write, destructive, idempotent| Tool { name, title: "Инструмент", description: "Что делает.", input_schema: json!({ "type": "object", "properties": {} }), write, destructive, idempotent };
        vec![t("summary", false, false, false), t("add", true, false, false), t("update", true, false, true), t("delete", true, true, false)]
    }

    fn run(line: &str) -> Value {
        let mut call = |name: &str, args: &Value| if name == "summary" { Ok(json!({ "got": args })) } else { Err("Проба заблокирована".to_string()) };
        serde_json::from_str(&handle(&SERVER, &tools(), line, "1.2.3", &mut call).unwrap()).unwrap()
    }

    #[test]
    fn protocol() {
        let r = run(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#);
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"], json!({ "name": "proba", "title": "Проба", "version": "1.2.3" }));
        assert_eq!(r["result"]["instructions"], "Подсказка для Claude.");
        let r = run(r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2099-01-01"}}"#);
        assert_eq!(r["result"]["protocolVersion"], VERSIONS[0], "незнакомая версия — самая новая своя");
        assert_eq!(handle(&SERVER, &tools(), r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, "1", &mut |_, _| Ok(json!(null))), None);

        let list = run(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#)["result"]["tools"].as_array().unwrap().clone();
        let hints = |name: &str| list.iter().find(|t| t["name"] == name).unwrap()["annotations"].clone();
        assert_eq!((hints("summary")["readOnlyHint"].clone(), hints("summary")["idempotentHint"].clone()), (json!(true), json!(true)));
        assert_eq!((hints("add")["readOnlyHint"].clone(), hints("add")["idempotentHint"].clone()), (json!(false), json!(false)));
        assert_eq!(hints("update")["idempotentHint"], true);
        assert_eq!((hints("delete")["destructiveHint"].clone(), hints("add")["destructiveHint"].clone()), (json!(true), json!(false)));

        let r = run(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"summary","arguments":{"x":1}}}"#);
        assert_eq!(r["result"]["isError"], false);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("\"x\": 1"));
        let r = run(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"add"}}"#);
        assert_eq!((r["result"]["isError"].clone(), r["result"]["content"][0]["text"].clone()), (json!(true), json!("Проба заблокирована")));
        assert_eq!(run(r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"nope"}}"#)["error"]["code"], -32602);
        assert_eq!(run(r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"add","arguments":[1]}}"#)["error"]["code"], -32602);
        assert_eq!(run(r#"{"jsonrpc":"2.0","id":8,"method":"ping"}"#)["result"], json!({}));
        assert_eq!(run(r#"{"jsonrpc":"2.0","id":9,"method":"sampling/createMessage"}"#)["error"]["code"], -32601);
        assert_eq!(run("не json")["error"]["code"], -32700);
        assert_eq!(run("[1]")["error"]["code"], -32600);
    }

    #[test]
    fn link_proof_depends_on_everything() {
        let token = "ab".repeat(32);
        let p = link_proof(&SERVER, &token, "app", &"01".repeat(32));
        assert_eq!(p.len(), 64);
        assert_eq!(p, link_proof(&SERVER, &token, "app", &"01".repeat(32)));
        assert_ne!(p, link_proof(&SERVER, &token, "relay", &"01".repeat(32)));
        assert_ne!(p, link_proof(&SERVER, &token, "app", &"02".repeat(32)));
        assert_ne!(p, link_proof(&SERVER, &"cd".repeat(32), "app", &"01".repeat(32)));
        let other = Server { link_personal: b"other-link", ..SERVER };
        assert_ne!(p, link_proof(&other, &token, "app", &"01".repeat(32)), "у каждого приложения своя строка");
        assert_eq!(link_proof(&SERVER, &"x".repeat(65), "app", "01"), "", "ключ длиннее 64 байт не подходит");
        assert_eq!(link_proof(&SERVER, "", "app", "01"), "");
    }

    /// Строки «Полки» и «Кармана» дают те же доказательства, что их коннекторы до kit (значения из
    /// образцов форматов): иначе посредник прежней версии и новое приложение не узнают друг друга.
    #[test]
    fn link_proof_of_apps_before_kit() {
        let polka = Server { link_personal: b"polka-link", ..SERVER };
        let karman = Server { link_personal: b"karman-link", ..SERVER };
        assert_eq!(link_proof(&polka, &"ab".repeat(32), "polka", &"01".repeat(32)), "554563ff54e07a95654feef3e997189a419816c1bb170198509f4221864d3c2c");
        assert_eq!(link_proof(&karman, &"ab".repeat(32), "karman", &"01".repeat(32)), "193ebbd2feeaf75b5acf7c5fe5949a362f74c3637c41da3073b0b2857a150794");
    }

    #[test]
    fn bundle_is_a_zip_with_a_valid_manifest() {
        let m = manifest(&SERVER, &EXT, &tools(), "1.2.3", r"C:\Users\Пётр\AppData\Local\Proba\proba.exe", r"C:\Users\Пётр\AppData\Roaming\proba", "win32");
        assert_eq!(m["name"], "proba");
        assert_eq!(m["server"]["entry_point"], "proba.exe");
        assert_eq!(m["server"]["mcp_config"]["args"][0], "--mcp");
        assert_eq!(m["server"]["mcp_config"]["env"]["PROBA_DIR"], r"C:\Users\Пётр\AppData\Roaming\proba");
        assert_eq!(m["tools"].as_array().unwrap().len(), 4);
        let data = bundle(&m, b"\x89PNG\r\n\x1a\nicon").unwrap();
        assert_eq!(&data[..4], b"PK\x03\x04");
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(data)).unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut z.by_name("manifest.json").unwrap(), &mut text).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), m);
        assert_eq!(z.by_name("icon.png").unwrap().size(), 12);
    }
}
