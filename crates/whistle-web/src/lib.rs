//! axum 管理面：REST API + WebSocket 实时推送 + 内嵌 UI。
//!
//! 对应 whistle 的 `lib/service` 与 Web UI 后端。共享代理内核的抓包存储与规则，
//! 支持在线编辑规则并热生效（无需重启）。

use std::sync::{Arc, RwLock};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
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
    Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/api/info", get(info_handler))
        .route("/api/traffic", get(list_handler).delete(clear_handler))
        .route("/api/traffic/{id}", get(get_handler))
        .route("/api/rules", put(put_rules_handler).get(get_rules_handler))
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
