//! axum 管理面：REST API + WebSocket 实时推送 + 内嵌 UI。
//!
//! 对应 whistle 的 `lib/service` 与 Web UI 后端。共享代理内核的抓包存储与规则，
//! 支持在线编辑规则并热生效（无需重启）。

mod whistle_compat;
pub mod whistle_store;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info};
use whistle_capture::CaptureStore;
use whistle_rules::RuleSet;

const INDEX_HTML: &str = include_str!("../ui/index.html");

/// 共享给 Web 与代理内核的状态。规则用 `RwLock` 以支持热更新。
#[derive(Clone)]
pub struct WebState {
    pub store: Arc<CaptureStore>,
    pub rules: Arc<RwLock<RuleSet>>,
    pub rules_text: Arc<RwLock<String>>,
    /// 代理监听地址 `host:port`，Composer 通过它回放请求（从而自动套用规则与抓包）。
    pub proxy_addr: String,
    /// UI 模式：`native`（内置精简界面）/ `whistle`（内嵌 whistle 原生前端 + 兼容后端）。
    pub ui_mode: String,
    /// whistle UI 的可编辑状态（规则分组/Values/开关），仅 whistle 模式使用。
    pub whistle: Arc<RwLock<whistle_store::WhistleData>>,
    /// 数据目录（持久化 whistle-ui.json）。
    pub data_dir: PathBuf,
}

impl WebState {
    /// 重新计算 whistle UI 的生效规则文本，热替换进代理规则集，并持久化。
    ///
    /// 任何对 `whistle` 数据的修改后都应调用本方法，使代理立即按新规则工作。
    pub fn whistle_apply_and_save(&self) {
        let (text, snapshot) = {
            let d = self.whistle.read().unwrap();
            (d.effective_text(), d.clone())
        };
        match RuleSet::parse(&text) {
            Ok(set) => {
                let count = set.len();
                *self.rules.write().unwrap() = set;
                *self.rules_text.write().unwrap() = text;
                info!(count, "whistle UI 规则已热更新");
            }
            Err(e) => {
                // 解析失败时不替换现有规则，仅记录（前端文本可能临时不合法）。
                info!(error = %e, "whistle UI 规则解析失败，保留旧规则");
            }
        }
        snapshot.save(&whistle_store::data_path(&self.data_dir));
    }
}

/// 启动 Web 管理面，监听 `addr`。
pub async fn serve(addr: std::net::SocketAddr, state: WebState) -> std::io::Result<()> {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "whistle-rs 管理界面已启动");
    axum::serve(listener, app).await
}

/// 构建路由（导出以便测试）。
pub fn router(state: WebState) -> Router {
    if state.ui_mode == "whistle" {
        // 内嵌 whistle 原生前端 + 兼容 cgi-bin 后端（实验性）。
        return whistle_compat::whistle_router(state);
    }
    Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/api/info", get(info_handler))
        .route("/api/traffic", get(list_handler).delete(clear_handler))
        .route("/api/traffic/{id}", get(get_handler))
        .route("/api/rules", put(put_rules_handler).get(get_rules_handler))
        .route("/api/compose", post(compose_handler))
        .route("/ws", get(ws_handler))
        .with_state(state)
}

async fn info_handler(State(s): State<WebState>) -> impl IntoResponse {
    Json(json!({
        "name": "whistle-rs",
        "version": env!("CARGO_PKG_VERSION"),
        "rules": s.rules.read().unwrap().len(),
        "capture": s.store.len(),
    }))
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<usize>,
}

async fn list_handler(State(s): State<WebState>, Query(q): Query<ListQuery>) -> impl IntoResponse {
    Json(s.store.list(q.limit.unwrap_or(500)))
}

async fn get_handler(State(s): State<WebState>, Path(id): Path<u64>) -> Response {
    match s.store.get(id) {
        Some(t) => Json(t).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn clear_handler(State(s): State<WebState>) -> impl IntoResponse {
    s.store.clear();
    Json(json!({ "ok": true }))
}

async fn get_rules_handler(State(s): State<WebState>) -> impl IntoResponse {
    let text = s.rules_text.read().unwrap().clone();
    Json(json!({ "text": text }))
}

/// 接收规则文本（纯文本 body），解析成功则热替换。
async fn put_rules_handler(State(s): State<WebState>, body: String) -> impl IntoResponse {
    match RuleSet::parse(&body) {
        Ok(set) => {
            let count = set.len();
            *s.rules.write().unwrap() = set;
            *s.rules_text.write().unwrap() = body;
            info!(count, "规则已热更新");
            Json(json!({ "ok": true, "count": count }))
        }
        Err(e) => Json(json!({ "ok": false, "error": e.to_string() })),
    }
}

/// Composer 请求体：回放一条 HTTP 请求。
#[derive(Deserialize)]
struct ComposeRequest {
    method: String,
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
}

/// Composer：把请求经由本机代理回放，从而自动套用规则并产生抓包记录。
async fn compose_handler(State(s): State<WebState>, Json(req): Json<ComposeRequest>) -> Response {
    // 目前仅支持明文 http://（经由代理以绝对形式转发）。
    if !req.url.starts_with("http://") {
        return Json(json!({ "ok": false, "error": "Composer 暂仅支持 http:// 目标" }))
            .into_response();
    }
    match replay_through_proxy(&s.proxy_addr, &req).await {
        Ok((status, headers, body)) => Json(json!({
            "ok": true,
            "status": status,
            "headers": headers,
            "body": body,
        }))
        .into_response(),
        Err(e) => Json(json!({ "ok": false, "error": e.to_string() })).into_response(),
    }
}

/// 手写极简 HTTP/1.1 客户端：连到代理，发绝对形式请求，读到 EOF。
/// 返回 `(状态码, 原始响应头块, 响应体 utf-8 lossy)`。
async fn replay_through_proxy(
    proxy_addr: &str,
    req: &ComposeRequest,
) -> std::io::Result<(u16, String, String)> {
    // 从 URL 推导 Host 头（authority 部分）。
    let authority = req
        .url
        .strip_prefix("http://")
        .unwrap_or(&req.url)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");

    let mut request = format!("{} {} HTTP/1.1\r\n", req.method, req.url);
    request.push_str(&format!("Host: {authority}\r\n"));
    request.push_str("Connection: close\r\n");
    for (name, value) in &req.headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    let body = req.body.as_deref().unwrap_or("");
    if !body.is_empty() {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    request.push_str(body);

    let mut stream = TcpStream::connect(proxy_addr).await?;
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    // 因 Connection: close，代理会在响应完整后关闭连接，读到 EOF 即可。
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;

    // 在首个 \r\n\r\n 处分割：前半为状态行 + 响应头，后半为响应体。
    let (head, body_bytes) = match buf.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(pos) => (&buf[..pos], &buf[pos + 4..]),
        None => (&buf[..], &b""[..]),
    };
    let head = String::from_utf8_lossy(head).into_owned();
    let body = String::from_utf8_lossy(body_bytes).into_owned();

    // 状态行形如 `HTTP/1.1 200 OK`，取第二段为状态码。
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);

    Ok((status, head, body))
}

async fn ws_handler(State(s): State<WebState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| ws_loop(socket, s))
}

/// 把抓包更新事件实时推给前端。
async fn ws_loop(mut socket: WebSocket, state: WebState) {
    let mut rx = state.store.subscribe();
    loop {
        match rx.recv().await {
            Ok(traffic) => {
                let Ok(text) = serde_json::to_string(&traffic) else {
                    continue;
                };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break; // 客户端断开
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                debug!(skipped = n, "WS 推送滞后，丢弃部分事件");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whistle_capture::Traffic;

    fn state() -> WebState {
        WebState {
            store: Arc::new(CaptureStore::new(100)),
            rules: Arc::new(RwLock::new(RuleSet::default())),
            rules_text: Arc::new(RwLock::new(String::new())),
            proxy_addr: "127.0.0.1:0".to_string(),
            ui_mode: "native".to_string(),
            whistle: Arc::new(RwLock::new(whistle_store::WhistleData::default())),
            data_dir: std::env::temp_dir(),
        }
    }

    #[test]
    fn put_rules_updates_shared_state() {
        let s = state();
        let body = "example.com host://1.2.3.4".to_string();
        let set = RuleSet::parse(&body).unwrap();
        assert_eq!(set.len(), 1);
        *s.rules.write().unwrap() = set;
        assert_eq!(s.rules.read().unwrap().len(), 1);
    }

    #[test]
    fn store_list_roundtrip() {
        let s = state();
        let id = s.store.next_id();
        s.store
            .upsert(Traffic::new(id, "http", "GET", "http://x/", "x", "c"));
        assert_eq!(s.store.list(10).len(), 1);
    }
}
