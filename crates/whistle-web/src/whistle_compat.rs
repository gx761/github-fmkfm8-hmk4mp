//! whistle 兼容适配层（实验性）。
//!
//! 直接服务 whistle 编译好的前端（`whistle-htdocs/`，见 VENDORED.md），并实现
//! whistle 前端所需的 `/cgi-bin/*` 后端协议的核心子集，把 whistle-rs 的抓包/规则
//! 数据映射成 whistle 的 session/规则 结构，让其原生 UI 跑起来。
//!
//! 状态：**iteration 1** —— 已实现 boot（`init`）与 Network 轮询（`get-data`）等核心端点；
//! 其余 cgi-bin 端点先返回 `{ec:0}` 占位。Rules/Values 等面板的完整保真度仍需在真实
//! 浏览器中联调。
//!
//! 协议要点（逆向自 whistle 2.10.4 `biz/webui`）：
//! - 前端启动调 `cgi-bin/init`；之后以游标 `startTime` 轮询 `cgi-bin/get-data`，
//!   返回 `{newIds, data:{id:session}, lastId, endId, frames, ...}`；
//! - session.id 形如 `"<startTime>-<n>"`，含 `req/res`（headers 对象 + body）与时序。

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::Router;
use include_dir::{include_dir, Dir};
use serde_json::{json, Map, Value};
use whistle_capture::Traffic;

use crate::WebState;

/// 内嵌的 whistle 前端静态资源（编译期打包）。
static HTDOCS: Dir = include_dir!("$CARGO_MANIFEST_DIR/whistle-htdocs");

/// 单次 get-data 返回的最大新条目数。
const COUNT: usize = 600;
/// 与所内嵌前端匹配的 whistle 版本号（避免前端版本不匹配提示）。
const WVERSION: &str = "2.10.4";

/// 构建 whistle 兼容 UI 的路由。
pub fn whistle_router(state: WebState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/cgi-bin/init", any(init))
        .route("/cgi-bin/get-data", any(get_data))
        .route("/cgi-bin/get-frames", any(get_frames))
        .route("/cgi-bin/server-info", any(server_info))
        .route("/cgi-bin/status", any(status))
        .route("/cgi-bin/rules/list", any(rules_list))
        .route("/cgi-bin/values/list", any(values_list))
        // 其余静态资源与未实现的 cgi-bin 走兜底。
        .route("/{*path}", get(static_or_stub).post(static_or_stub))
        .with_state(state)
}

/// 首页：内嵌 index.html。
async fn index() -> Response {
    serve_embedded("index.html")
        .unwrap_or_else(|| (axum::http::StatusCode::NOT_FOUND, "no index").into_response())
}

/// 静态资源；`cgi-bin/*` 未实现端点返回 `{ec:0}` 占位（避免前端 404 崩溃）。
async fn static_or_stub(Path(path): Path<String>) -> Response {
    if path.starts_with("cgi-bin/") {
        return axum::Json(json!({ "ec": 0 })).into_response();
    }
    serve_embedded(&path)
        .unwrap_or_else(|| (axum::http::StatusCode::NOT_FOUND, "not found").into_response())
}

fn serve_embedded(path: &str) -> Option<Response> {
    let file = HTDOCS.get_file(path)?;
    let ct = content_type(path);
    Some(
        (
            [(axum::http::header::CONTENT_TYPE, ct)],
            file.contents().to_vec(),
        )
            .into_response(),
    )
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

/// `cgi-bin/init`：前端启动数据。
async fn init(State(s): State<WebState>) -> axum::Json<Value> {
    axum::Json(json!({
        "ec": 0,
        "version": WVERSION,
        "wName": "whistle-rs",
        "supportH2": true,
        "enableHttp2": true,
        "interceptHttpsConnects": true,
        "server": server_obj(),
        "clientId": "whistle-rs",
        "clientIp": "127.0.0.1",
        "lastDataId": "0",
        "rules": rules_payload(&s),
        "values": values_payload(),
        "plugins": {},
        "disabledPlugins": {},
    }))
}

/// `cgi-bin/get-data`：Network 轮询。
async fn get_data(
    State(s): State<WebState>,
    Query(q): Query<HashMap<String, String>>,
) -> axum::Json<Value> {
    // 升序（按 id）排列全部条目。
    let mut all = s.store.list(2000);
    all.reverse();

    let end_id = all.last().map(session_id);
    let cursor = q.get("startTime").and_then(|x| parse_cursor(x));

    let news: Vec<&Traffic> = match cursor {
        Some(c) => all.iter().filter(|t| t.id > c).take(COUNT).collect(),
        None => {
            let n = all.len();
            all[n.saturating_sub(COUNT)..].iter().collect()
        }
    };

    let mut data = Map::new();
    for t in &news {
        data.insert(session_id(t), session_json(t));
    }
    // 前端请求具体 ids（如取详情 body）时也带上。
    if let Some(ids) = q.get("ids") {
        for idstr in ids.split(',').filter(|x| !x.is_empty()) {
            if let Some(num) = parse_cursor(idstr) {
                if let Some(t) = all.iter().find(|t| t.id == num) {
                    data.insert(session_id(t), session_json(t));
                }
            }
        }
    }

    let new_ids: Vec<String> = news.iter().map(|t| session_id(t)).collect();
    let last_id = new_ids.last().cloned().or_else(|| end_id.clone());

    // WebSocket 帧（当前选中会话）。
    let frames = q
        .get("curReqId")
        .and_then(|cur| frames_for(&all, cur))
        .unwrap_or_else(|| Value::Array(vec![]));

    axum::Json(json!({
        "ec": 0,
        "version": WVERSION,
        "server": server_obj(),
        "newIds": new_ids,
        "data": Value::Object(data),
        "lastId": last_id,
        "endId": end_id,
        "hasNew": false,
        "frames": frames,
        "svrLog": [],
        "plugins": {},
        "list": [],
        "enabledCount": 0,
        "interceptHttpsConnects": true,
        "enableHttp2": true,
    }))
}

async fn get_frames(
    State(s): State<WebState>,
    Query(q): Query<HashMap<String, String>>,
) -> axum::Json<Value> {
    let all = s.store.list(2000);
    let frames = q
        .get("curReqId")
        .and_then(|cur| frames_for(&all, cur))
        .unwrap_or_else(|| Value::Array(vec![]));
    axum::Json(json!({ "ec": 0, "frames": frames }))
}

async fn server_info() -> axum::Json<Value> {
    axum::Json(json!({ "ec": 0, "server": server_obj() }))
}

async fn status() -> axum::Json<Value> {
    axum::Json(json!({ "ec": 0 }))
}

async fn rules_list(State(s): State<WebState>) -> axum::Json<Value> {
    axum::Json(json!({ "ec": 0, "rules": rules_payload(&s) }))
}

async fn values_list() -> axum::Json<Value> {
    axum::Json(json!({ "ec": 0, "values": values_payload() }))
}

// ---- 映射辅助 ----

fn server_obj() -> Value {
    json!({ "name": "whistle-rs", "version": WVERSION, "nodeVersion": "rust" })
}

/// 规则面板载荷：把 whistle-rs 的单一规则文本呈现为一个 Default 分组。
fn rules_payload(s: &WebState) -> Value {
    let text = s.rules_text.read().unwrap().clone();
    json!({
        "defaultRules": text,
        "disabledDefaultRules": false,
        "list": [],
    })
}

fn values_payload() -> Value {
    json!({ "list": [] })
}

/// session id：`"<startTime>-<id>"`，与 whistle 一致。
fn session_id(t: &Traffic) -> String {
    format!("{}-{}", t.start_time, t.id)
}

/// 从 startTime 游标或 id 串中解析出 whistle-rs 数值 id（末段）。
fn parse_cursor(s: &str) -> Option<u64> {
    if s.is_empty() || s == "0" || s == "-1" || s == "-2" || s == "-3" {
        return None;
    }
    s.rsplit('-').next().and_then(|n| n.parse::<u64>().ok())
}

fn headers_obj(hs: &[whistle_capture::Header]) -> Value {
    let mut m = Map::new();
    for h in hs {
        m.insert(h.name.to_ascii_lowercase(), Value::String(h.value.clone()));
    }
    Value::Object(m)
}

fn raw_names(hs: &[whistle_capture::Header]) -> Value {
    let mut m = Map::new();
    for h in hs {
        m.insert(h.name.to_ascii_lowercase(), Value::String(h.name.clone()));
    }
    Value::Object(m)
}

/// 把一条 Traffic 映射为 whistle session 对象。
fn session_json(t: &Traffic) -> Value {
    let dur = t.duration_ms.unwrap_or(0);
    let end = t.start_time + dur;
    let is_https = matches!(t.protocol.as_str(), "https" | "wss" | "tunnel");
    json!({
        "id": session_id(t),
        "url": t.url,
        "method": t.method,
        "httpVersion": "1.1",
        "isHttps": is_https,
        "startTime": t.start_time,
        "dnsTime": t.start_time,
        "requestTime": t.start_time,
        "responseTime": end,
        "endTime": end,
        "req": {
            "method": t.method,
            "httpVersion": "1.1",
            "headers": headers_obj(&t.req_headers),
            "rawHeaderNames": raw_names(&t.req_headers),
            "size": t.req_body_size,
            "body": t.req_body.clone().unwrap_or_default(),
        },
        "res": {
            "statusCode": t.status.unwrap_or(0),
            "statusMessage": "",
            "httpVersion": "1.1",
            "headers": headers_obj(&t.res_headers),
            "rawHeaderNames": raw_names(&t.res_headers),
            "size": t.res_body_size,
            "body": t.res_body.clone().unwrap_or_default(),
        },
        "rules": {},
        "rulesHeaders": {},
        "frames": Value::Array(vec![]),
        "version": WVERSION,
    })
}

/// 为某个会话构造 whistle frame 数组（WebSocket 消息）。
fn frames_for(all: &[Traffic], cur: &str) -> Option<Value> {
    let num = parse_cursor(cur)?;
    let t = all.iter().find(|t| t.id == num)?;
    if t.ws_messages.is_empty() {
        return Some(Value::Array(vec![]));
    }
    let frames: Vec<Value> = t
        .ws_messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            json!({
                "reqId": cur,
                "frameId": format!("{}-{}", t.start_time, i),
                "isClient": m.dir == "send",
                "length": m.size,
                "opcode": m.opcode,
                "text": m.text.clone().unwrap_or_default(),
                "closed": m.opcode == 0x8,
            })
        })
        .collect();
    Some(Value::Array(frames))
}
