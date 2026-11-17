//! Запросы к облаку: таймауты, пределы ответа и понятные ошибки.

use kit_core::cloud::CloudFail;
use kit_core::{Error, Result};
use serde::de::DeserializeOwned;
use std::time::Duration;

/// Сейчас: секунды Unix.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn fail(kind: CloudFail, msg: impl Into<String>) -> Error {
    Error::Cloud(kind, msg.into())
}

pub fn auth_expired() -> Error {
    fail(CloudFail::Auth, "Вход в облако устарел или доступ отозван — войдите снова")
}

pub fn no_space() -> Error {
    fail(CloudFail::Space, "В облаке не хватает места — освободите место на диске")
}

pub fn server(code: u16) -> Error {
    if code == 429 {
        return fail(CloudFail::Server, "Облако просит подождать: слишком много запросов. Обмен повторится позже");
    }
    fail(CloudFail::Server, format!("Облако ответило ошибкой {code} — обмен повторится позже"))
}

pub fn odd() -> Error {
    fail(CloudFail::Server, "Облако ответило непонятно — обмен повторится позже")
}

#[derive(Clone, Copy)]
pub enum Body<'a> {
    Empty,
    Bytes(&'a [u8]),
    Json(&'a [u8]),
    /// Запрос WebDAV (PROPFIND).
    Xml(&'a [u8]),
    Form(&'a [(&'a str, &'a str)]),
}

pub struct Resp {
    pub status: u16,
    pub body: Vec<u8>,
    pub location: Option<String>,
}

impl Resp {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|_| odd())
    }

    /// Код ошибки из ответа: поле `error` (Яндекс, OAuth) или причина ошибки Google Drive.
    pub fn error_code(&self) -> String {
        let v: serde_json::Value = serde_json::from_slice(&self.body).unwrap_or_default();
        if let Some(s) = v.get("error").and_then(|e| e.as_str()) {
            return s.to_string();
        }
        v.pointer("/error/errors/0/reason").and_then(|r| r.as_str()).unwrap_or_default().to_string()
    }
}

pub struct Http {
    agent: ureq::Agent,
}

fn net(e: ureq::Error) -> Error {
    match e {
        ureq::Error::BodyExceedsLimit(n) => fail(CloudFail::Format, format!("файл в облаке больше допустимого ({n} байт)")),
        e => fail(CloudFail::Offline, format!("Нет связи с облаком ({e}) — проверьте интернет. Обмен повторится позже")),
    }
}

impl Http {
    /// `agent` — имя приложения в заголовке User-Agent: «Karman».
    pub fn new(agent: &str) -> Http {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_resolve(Some(Duration::from_secs(15)))
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_send_request(Some(Duration::from_secs(60)))
            .timeout_recv_response(Some(Duration::from_secs(90)))
            // Вложение в 15 МБ по медленной мобильной связи идёт долго.
            .timeout_send_body(Some(Duration::from_secs(15 * 60)))
            .timeout_recv_body(Some(Duration::from_secs(15 * 60)))
            .user_agent(agent)
            // WebDAV: PROPFIND и MKCOL.
            .allow_non_standard_methods(true)
            .build()
            .into();
        Http { agent }
    }

    /// Запрос; ответ — не больше `max` байт.
    pub fn send(&self, method: &str, url: &str, headers: &[(&str, &str)], body: Body, max: usize) -> Result<Resp> {
        let r = match method {
            "GET" | "DELETE" => {
                let mut b = if method == "GET" { self.agent.get(url) } else { self.agent.delete(url) };
                for (k, v) in headers {
                    b = b.header(*k, *v);
                }
                b.call()
            }
            "POST" | "PUT" | "PATCH" => {
                let mut b = match method {
                    "POST" => self.agent.post(url),
                    "PUT" => self.agent.put(url),
                    _ => self.agent.patch(url),
                };
                for (k, v) in headers {
                    b = b.header(*k, *v);
                }
                match body {
                    Body::Empty => b.send_empty(),
                    Body::Bytes(d) => b.header("Content-Type", "application/octet-stream").send(d),
                    Body::Json(d) => b.header("Content-Type", "application/json; charset=UTF-8").send(d),
                    Body::Xml(d) => b.header("Content-Type", "application/xml; charset=utf-8").send(d),
                    Body::Form(f) => b.send_form(f.iter().copied()),
                }
            }
            // WebDAV: PROPFIND, MKCOL.
            _ => {
                let mut b = ureq::http::Request::builder().method(method).uri(url);
                for (k, v) in headers {
                    b = b.header(*k, *v);
                }
                let data: &[u8] = match body {
                    Body::Empty => &[],
                    Body::Xml(d) => {
                        b = b.header("Content-Type", "application/xml; charset=utf-8");
                        d
                    }
                    Body::Bytes(d) | Body::Json(d) => d,
                    Body::Form(_) => return Err(odd()),
                };
                let req = b.body(data).map_err(|_| fail(CloudFail::Format, "Неверный адрес облака"))?;
                self.agent.run(req)
            }
        };
        let mut resp = r.map_err(net)?;
        let status = resp.status().as_u16();
        let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
        let body = resp.body_mut().with_config().limit(max as u64).read_to_vec().map_err(net)?;
        Ok(Resp { status, body, location })
    }
}

/// Кодирование для адреса: всё, кроме латинских букв, цифр и `-_.~`.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_paths_and_queries() {
        assert_eq!(enc("app:/e/aaaa-1-3.kev"), "app%3A%2Fe%2Faaaa-1-3.kev");
        assert_eq!(enc("name='Карман'"), "name%3D%27%D0%9A%D0%B0%D1%80%D0%BC%D0%B0%D0%BD%27");
    }

    #[test]
    fn error_codes_from_both_clouds() {
        let r = |b: &str| Resp { status: 400, body: b.as_bytes().to_vec(), location: None };
        assert_eq!(r(r#"{"error":"authorization_pending"}"#).error_code(), "authorization_pending");
        assert_eq!(r(r#"{"error":{"code":403,"errors":[{"reason":"storageQuotaExceeded"}]}}"#).error_code(), "storageQuotaExceeded");
        assert_eq!(r("not json").error_code(), "");
    }
}
