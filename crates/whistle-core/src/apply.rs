//! 规则应用（inspectors）。
//!
//! 将 [`whistle_rules`] 求值得到的操作应用到请求/响应上。M2 覆盖的 P0 协议：
//! `host`、`redirect`、`file`/`rawfile`、`statusCode`、`reqHeaders`/`resHeaders`、
//! `reqType`/`resType`。
//!
//! 说明：当前实现内置于 `whistle-core`；后续里程碑将抽出独立的 `whistle-inspectors`
//! crate（见 `docs/03-module-mapping.md`）。

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE, LOCATION};
use hyper::{Response, StatusCode};
use tracing::debug;
use whistle_rules::{all_values, last_value, Operation};

/// 统一错误盒子。
pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;
/// 出站响应 body 类型。
pub(crate) type ResBody = BoxBody<Bytes, BoxError>;

/// 请求阶段处理结果。
pub(crate) enum RequestAction {
    /// 直接返回本地响应（mock），不转发上游。
    Mock(Response<ResBody>),
    /// 转发到上游（host/port 可能已被 `host://` 覆盖）。
    Forward { host: String, port: u16 },
}

/// 根据操作决定请求阶段行为（是否 mock、转发目标）。
pub(crate) fn request_action(ops: &[Operation], host: &str, port: u16) -> RequestAction {
    // mock 优先级：file/rawfile > redirect > statusCode。
    if let Some(path) = last_value(ops, "file").or_else(|| last_value(ops, "rawfile")) {
        return RequestAction::Mock(serve_file(path));
    }
    if let Some(target) = last_value(ops, "redirect") {
        return RequestAction::Mock(redirect(target));
    }
    if let Some(code) = last_value(ops, "statusCode") {
        return RequestAction::Mock(status_mock(code));
    }

    // host 覆盖上游目标。
    let (mut up_host, mut up_port) = (host.to_string(), port);
    if let Some(v) = last_value(ops, "host") {
        let (h, p) = parse_host(v);
        if !h.is_empty() {
            up_host = h;
        }
        if let Some(p) = p {
            up_port = p;
        }
    }
    RequestAction::Forward {
        host: up_host,
        port: up_port,
    }
}

/// 应用请求阶段的首部类操作。
pub(crate) fn apply_request_headers(headers: &mut HeaderMap, ops: &[Operation]) {
    for v in all_values(ops, "reqHeaders") {
        set_headers_from_pairs(headers, v);
    }
    if let Some(t) = last_value(ops, "reqType") {
        set_header(headers, CONTENT_TYPE.as_str(), &mime_of(t));
    }
    if let Some(ua) = last_value(ops, "ua") {
        set_header(headers, "user-agent", ua);
    }
    if let Some(r) = last_value(ops, "referer") {
        set_header(headers, "referer", r);
    }
}

/// 应用响应阶段操作（转发场景）。
pub(crate) fn apply_response(parts: &mut hyper::http::response::Parts, ops: &[Operation]) {
    for v in all_values(ops, "resHeaders") {
        set_headers_from_pairs(&mut parts.headers, v);
    }
    if let Some(t) = last_value(ops, "resType") {
        set_header(&mut parts.headers, CONTENT_TYPE.as_str(), &mime_of(t));
    }
}

/// 解析 `host://` 的值为 `(host, Option<port>)`。
fn parse_host(value: &str) -> (String, Option<u16>) {
    // 容忍 `host://1.2.3.4:8080` 里再带 scheme 的少见写法。
    let value = value.rsplit("://").next().unwrap_or(value);
    match value.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()),
        None => (value.to_string(), None),
    }
}

/// 由 `a=1&b=2` 形式设置（覆盖）多个首部。
fn set_headers_from_pairs(headers: &mut HeaderMap, pairs: &str) {
    for pair in pairs.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        set_header(headers, name.trim(), value.trim());
    }
}

/// 设置（覆盖）单个首部；非法名/值会被跳过。
fn set_header(headers: &mut HeaderMap, name: &str, value: &str) {
    match (
        HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        (Ok(n), Ok(v)) => {
            headers.insert(n, v);
        }
        _ => debug!(name, value, "跳过非法首部"),
    }
}

/// 短名 → MIME；含 `/` 则视为完整 MIME。
fn mime_of(t: &str) -> String {
    if t.contains('/') {
        return t.to_string();
    }
    match t {
        "html" => "text/html",
        "json" => "application/json",
        "js" | "javascript" => "application/javascript",
        "css" => "text/css",
        "xml" => "application/xml",
        "text" | "txt" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        other => other,
    }
    .to_string()
}

/// 由文件扩展名猜测 content-type。
fn guess_mime_by_ext(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// `file`/`rawfile`：读取本地文件作为响应；缺失则 404。
fn serve_file(path: &str) -> Response<ResBody> {
    match std::fs::read(path) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, guess_mime_by_ext(path))
            .header(CONTENT_LENGTH, bytes.len())
            .body(full_body(bytes))
            .expect("构造文件响应不应失败"),
        Err(e) => {
            debug!(path, %e, "file 规则：读取失败");
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(text_body(&format!("文件未找到: {path}")))
                .expect("构造 404 不应失败")
        }
    }
}

/// `redirect`：302 跳转。
fn redirect(target: &str) -> Response<ResBody> {
    match HeaderValue::from_str(target) {
        Ok(loc) => Response::builder()
            .status(StatusCode::FOUND)
            .header(LOCATION, loc)
            .body(empty_body())
            .expect("构造 302 不应失败"),
        Err(_) => Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(text_body("redirect 目标非法"))
            .expect("构造 400 不应失败"),
    }
}

/// `statusCode`：以指定状态码 mock（空 body）。
fn status_mock(code: &str) -> Response<ResBody> {
    let status = code
        .parse::<u16>()
        .ok()
        .and_then(|c| StatusCode::from_u16(c).ok())
        .unwrap_or(StatusCode::OK);
    Response::builder()
        .status(status)
        .body(empty_body())
        .expect("构造状态 mock 不应失败")
}

pub(crate) fn empty_body() -> ResBody {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

pub(crate) fn text_body(s: &str) -> ResBody {
    Full::new(Bytes::from(s.to_string()))
        .map_err(|never| match never {})
        .boxed()
}

fn full_body(bytes: Vec<u8>) -> ResBody {
    Full::new(Bytes::from(bytes))
        .map_err(|never| match never {})
        .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use whistle_rules::RuleSet;

    fn ops(rule: &str, scheme: &str, host: &str, path: &str) -> Vec<Operation> {
        let rs = RuleSet::parse(rule).unwrap();
        rs.match_request(&whistle_rules::MatchInput::new(scheme, host, path))
    }

    #[test]
    fn host_override_with_port() {
        let o = ops(
            "example.com host://1.2.3.4:9000",
            "http",
            "example.com",
            "/",
        );
        match request_action(&o, "example.com", 80) {
            RequestAction::Forward { host, port } => {
                assert_eq!(host, "1.2.3.4");
                assert_eq!(port, 9000);
            }
            _ => panic!("应为 Forward"),
        }
    }

    #[test]
    fn host_override_keeps_port_when_absent() {
        let o = ops("example.com host://1.2.3.4", "http", "example.com", "/");
        match request_action(&o, "example.com", 80) {
            RequestAction::Forward { host, port } => {
                assert_eq!(host, "1.2.3.4");
                assert_eq!(port, 80);
            }
            _ => panic!("应为 Forward"),
        }
    }

    #[test]
    fn status_code_mocks() {
        let o = ops("example.com statusCode://418", "http", "example.com", "/");
        match request_action(&o, "example.com", 80) {
            RequestAction::Mock(resp) => assert_eq!(resp.status().as_u16(), 418),
            _ => panic!("应为 Mock"),
        }
    }

    #[test]
    fn redirect_sets_location() {
        let o = ops(
            "example.com redirect://http://localhost/x",
            "http",
            "example.com",
            "/",
        );
        match request_action(&o, "example.com", 80) {
            RequestAction::Mock(resp) => {
                assert_eq!(resp.status(), StatusCode::FOUND);
                assert_eq!(resp.headers()[LOCATION], "http://localhost/x");
            }
            _ => panic!("应为 Mock"),
        }
    }

    #[test]
    fn req_headers_applied() {
        let o = ops(
            "example.com reqHeaders://x-a=1&x-b=2",
            "http",
            "example.com",
            "/",
        );
        let mut h = HeaderMap::new();
        apply_request_headers(&mut h, &o);
        assert_eq!(h["x-a"], "1");
        assert_eq!(h["x-b"], "2");
    }

    #[test]
    fn missing_file_is_404() {
        let o = ops(
            "example.com file:///nonexistent/zzz.json",
            "http",
            "example.com",
            "/",
        );
        match request_action(&o, "example.com", 80) {
            RequestAction::Mock(resp) => assert_eq!(resp.status(), StatusCode::NOT_FOUND),
            _ => panic!("应为 Mock"),
        }
    }
}
