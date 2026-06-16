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
    // reqCookies：合并到 Cookie 头。
    let cookies = all_values(ops, "reqCookies");
    if !cookies.is_empty() {
        merge_cookie_header(headers, &cookies);
    }
    // auth://user:pass → Authorization: Basic。
    if let Some(cred) = last_value(ops, "auth") {
        set_header(
            headers,
            "authorization",
            &format!("Basic {}", base64(cred.as_bytes())),
        );
    }
    // reqCors://origin → 设置 Origin 头（模拟跨域）。
    if let Some(origin) = last_value(ops, "reqCors") {
        set_header(headers, "origin", origin);
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
    // resCookies：每对追加一个 Set-Cookie。
    for v in all_values(ops, "resCookies") {
        for pair in v.split('&').filter(|p| !p.is_empty()) {
            if let Ok(val) = HeaderValue::from_str(pair) {
                parts.headers.append(hyper::header::SET_COOKIE, val);
            }
        }
    }
    if let Some(name) = last_value(ops, "attachment") {
        set_header(
            &mut parts.headers,
            "content-disposition",
            &format!("attachment; filename=\"{name}\""),
        );
    }
    // resCors://origin|* → 注入 CORS 响应头。
    if let Some(origin) = last_value(ops, "resCors") {
        set_header(&mut parts.headers, "access-control-allow-origin", origin);
        set_header(
            &mut parts.headers,
            "access-control-allow-methods",
            "GET, POST, PUT, DELETE, PATCH, OPTIONS, HEAD",
        );
        set_header(&mut parts.headers, "access-control-allow-headers", "*");
        if origin != "*" {
            set_header(
                &mut parts.headers,
                "access-control-allow-credentials",
                "true",
            );
        }
    }
}

/// 标准 base64 编码（无外部依赖）。
fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// `urlParams://a=1&b=2`：向请求 path_and_query 合并/覆盖查询参数。
pub(crate) fn merge_url_params(pq: &str, ops: &[Operation]) -> String {
    let additions = all_values(ops, "urlParams");
    if additions.is_empty() {
        return pq.to_string();
    }
    let (path, query) = match pq.split_once('?') {
        Some((p, q)) => (p, q),
        None => (pq, ""),
    };
    // 保留顺序的去重：后写覆盖先写。
    let mut params: Vec<(String, String)> = Vec::new();
    let put = |k: String, v: String, params: &mut Vec<(String, String)>| {
        if let Some(slot) = params.iter_mut().find(|(ek, _)| *ek == k) {
            slot.1 = v;
        } else {
            params.push((k, v));
        }
    };
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        put(k.to_string(), v.to_string(), &mut params);
    }
    for add in additions {
        for pair in add.split('&').filter(|s| !s.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            put(k.to_string(), v.to_string(), &mut params);
        }
    }
    let q = params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    format!("{path}?{q}")
}

/// 合并若干 `a=1&b=2` 到现有 Cookie 头。
fn merge_cookie_header(headers: &mut HeaderMap, additions: &[&str]) {
    let mut parts: Vec<String> = Vec::new();
    if let Some(existing) = headers.get(hyper::header::COOKIE) {
        if let Ok(s) = existing.to_str() {
            parts.push(s.to_string());
        }
    }
    for a in additions {
        for pair in a.split('&').filter(|p| !p.is_empty()) {
            parts.push(pair.trim().to_string());
        }
    }
    if let Ok(v) = HeaderValue::from_str(&parts.join("; ")) {
        headers.insert(hyper::header::COOKIE, v);
    }
}

/// `reqDelay`/`resDelay`：取延迟（毫秒）。
pub(crate) fn req_delay(ops: &[Operation]) -> Option<std::time::Duration> {
    delay_of(ops, "reqDelay")
}
pub(crate) fn res_delay(ops: &[Operation]) -> Option<std::time::Duration> {
    delay_of(ops, "resDelay")
}
fn delay_of(ops: &[Operation], proto: &str) -> Option<std::time::Duration> {
    last_value(ops, proto)
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
}

/// `method://`：覆盖请求方法。
pub(crate) fn override_method(ops: &[Operation], parts: &mut hyper::http::request::Parts) {
    if let Some(m) = last_value(ops, "method") {
        if let Ok(method) = hyper::Method::from_bytes(m.to_ascii_uppercase().as_bytes()) {
            parts.method = method;
        }
    }
}

/// `replaceStatus://` / `statusCode://`（转发场景）：覆盖响应状态码。
pub(crate) fn override_status(ops: &[Operation], status: &mut StatusCode) {
    let code = last_value(ops, "replaceStatus").or_else(|| last_value(ops, "statusCode"));
    if let Some(c) = code
        .and_then(|c| c.parse::<u16>().ok())
        .and_then(|c| StatusCode::from_u16(c).ok())
    {
        *status = c;
    }
}

/// 响应体改写协议集合。
const RES_BODY_PROTOS: &[&str] = &[
    "resBody",
    "htmlBody",
    "jsBody",
    "cssBody",
    "resReplace",
    "resPrepend",
    "resAppend",
    "htmlPrepend",
    "htmlAppend",
    "jsPrepend",
    "jsAppend",
    "cssPrepend",
    "cssAppend",
];
/// 请求体改写协议集合。
const REQ_BODY_PROTOS: &[&str] = &["reqBody", "reqReplace", "reqPrepend", "reqAppend"];

pub(crate) fn has_res_body_rewrite(ops: &[Operation]) -> bool {
    ops.iter()
        .any(|o| RES_BODY_PROTOS.contains(&o.protocol.as_str()))
}
pub(crate) fn has_req_body_rewrite(ops: &[Operation]) -> bool {
    ops.iter()
        .any(|o| REQ_BODY_PROTOS.contains(&o.protocol.as_str()))
}

/// 改写响应体（UTF-8 文本处理）。
pub(crate) fn rewrite_res_body(bytes: &[u8], ops: &[Operation]) -> Vec<u8> {
    rewrite_body(
        bytes,
        ops,
        &["resBody", "htmlBody", "jsBody", "cssBody"],
        &["resReplace"],
        &["resPrepend", "htmlPrepend", "jsPrepend", "cssPrepend"],
        &["resAppend", "htmlAppend", "jsAppend", "cssAppend"],
    )
}

/// 改写请求体（UTF-8 文本处理）。
pub(crate) fn rewrite_req_body(bytes: &[u8], ops: &[Operation]) -> Vec<u8> {
    rewrite_body(
        bytes,
        ops,
        &["reqBody"],
        &["reqReplace"],
        &["reqPrepend"],
        &["reqAppend"],
    )
}

fn rewrite_body(
    bytes: &[u8],
    ops: &[Operation],
    whole: &[&str],
    replace: &[&str],
    prepend: &[&str],
    append: &[&str],
) -> Vec<u8> {
    let mut s = String::from_utf8_lossy(bytes).into_owned();
    // 整体替换（取最后一次出现）。
    for p in whole {
        if let Some(v) = last_value(ops, p) {
            s = v.to_string();
        }
    }
    // from=to 字符串替换。
    for p in replace {
        for v in all_values(ops, p) {
            if let Some((from, to)) = v.split_once('=') {
                s = s.replace(from, to);
            }
        }
    }
    // 前置 / 后置。
    let mut pre = String::new();
    for p in prepend {
        for v in all_values(ops, p) {
            pre.push_str(v);
        }
    }
    let mut post = String::new();
    for p in append {
        for v in all_values(ops, p) {
            post.push_str(v);
        }
    }
    format!("{pre}{s}{post}").into_bytes()
}

/// 改写 body 后修正 Content-Length 并去掉分块编码。
pub(crate) fn set_content_length(headers: &mut HeaderMap, len: usize) {
    headers.remove(hyper::header::TRANSFER_ENCODING);
    if let Ok(v) = HeaderValue::from_str(&len.to_string()) {
        headers.insert(CONTENT_LENGTH, v);
    }
}

/// `proxy://`(或 `http-proxy://`)：上游 HTTP 代理地址 `(host, port)`。
pub(crate) fn upstream_proxy(ops: &[Operation]) -> Option<(String, u16)> {
    let v = last_value(ops, "proxy").or_else(|| last_value(ops, "http-proxy"))?;
    let v = v.rsplit("://").next().unwrap_or(v);
    match v.rsplit_once(':') {
        Some((h, p)) => Some((h.to_string(), p.parse().unwrap_or(80))),
        None => Some((v.to_string(), 80)),
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

pub(crate) fn full_body(bytes: Vec<u8>) -> ResBody {
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
    fn res_body_rewrite_combines() {
        let o = ops(
            "example.com resReplace://foo=bar htmlPrepend://<x> htmlAppend://</x>",
            "http",
            "example.com",
            "/",
        );
        assert!(has_res_body_rewrite(&o));
        let out = rewrite_res_body(b"foo middle foo", &o);
        assert_eq!(String::from_utf8(out).unwrap(), "<x>bar middle bar</x>");
    }

    #[test]
    fn res_body_whole_replace() {
        let o = ops("example.com resBody://hello", "http", "example.com", "/");
        let out = rewrite_res_body(b"original", &o);
        assert_eq!(String::from_utf8(out).unwrap(), "hello");
    }

    #[test]
    fn method_and_status_and_delay() {
        let o = ops(
            "example.com method://post replaceStatus://201 resDelay://50",
            "http",
            "example.com",
            "/",
        );
        let mut parts = hyper::Request::new(()).into_parts().0;
        override_method(&o, &mut parts);
        assert_eq!(parts.method, hyper::Method::POST);
        let mut st = StatusCode::OK;
        override_status(&o, &mut st);
        assert_eq!(st, StatusCode::CREATED);
        assert_eq!(res_delay(&o), Some(std::time::Duration::from_millis(50)));
    }

    #[test]
    fn base64_basic() {
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    #[test]
    fn url_params_merge_and_override() {
        let o = ops(
            "example.com urlParams://b=2&a=9",
            "http",
            "example.com",
            "/",
        );
        // 已有 a=1 被覆盖为 9，新增 b=2。
        assert_eq!(merge_url_params("/p?a=1", &o), "/p?a=9&b=2");
        assert_eq!(merge_url_params("/p", &o), "/p?b=2&a=9");
    }

    #[test]
    fn auth_and_cors_headers() {
        let o = ops("example.com auth://user:pass", "http", "example.com", "/");
        let mut h = HeaderMap::new();
        apply_request_headers(&mut h, &o);
        assert_eq!(h["authorization"], "Basic dXNlcjpwYXNz");

        let o2 = ops("example.com resCors://*", "http", "example.com", "/");
        let mut parts = hyper::Response::new(()).into_parts().0;
        apply_response(&mut parts, &o2);
        assert_eq!(parts.headers["access-control-allow-origin"], "*");
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
