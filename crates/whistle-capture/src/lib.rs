//! 抓包数据模型、存储与实时事件流。
//!
//! 对应 whistle 的 Network 面板数据层。提供：
//! - [`Traffic`]：单条流量记录；
//! - [`CaptureStore`]：内存环形缓冲 + 实时事件广播（供 Web UI 订阅）。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// 一个 HTTP 头部键值对。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub name: String,
    pub value: String,
}

/// 一条 WebSocket 消息记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsMessage {
    /// 方向：`send`（客户端→服务端）/ `recv`（服务端→客户端）。
    pub dir: String,
    /// WebSocket 操作码（0x1 文本，0x2 二进制，0x8 关闭，0x9 ping，0xA pong）。
    pub opcode: u8,
    /// 文本预览（文本帧时；可能截断）。
    pub text: Option<String>,
    /// 该消息原始负载字节数。
    pub size: u64,
}

/// 单条流量记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Traffic {
    /// 自增唯一 id。
    pub id: u64,
    /// 协议类别：`http` / `https` / `tunnel` / `ws`。
    pub protocol: String,
    pub method: String,
    /// 完整 URL。
    pub url: String,
    pub host: String,
    /// 客户端地址。
    pub client: String,
    /// 响应状态码（未完成时为 None）。
    pub status: Option<u16>,
    pub req_headers: Vec<Header>,
    pub res_headers: Vec<Header>,
    /// 请求开始时间（Unix 毫秒）。
    pub start_time: u64,
    /// 端到端耗时（毫秒，未完成时为 None）。
    pub duration_ms: Option<u64>,
    /// 命中的规则操作（如 `host://1.2.3.4`）。
    pub rules: Vec<String>,
    /// 请求体预览（UTF-8 lossy，可能截断）。
    pub req_body: Option<String>,
    /// 响应体预览（UTF-8 lossy，可能截断）。
    pub res_body: Option<String>,
    /// 请求体原始字节数。
    pub req_body_size: u64,
    /// 响应体原始字节数。
    pub res_body_size: u64,
    /// 响应体预览是否被截断。
    pub res_body_truncated: bool,
    /// WebSocket 消息记录（仅 ws/wss 流量）。
    pub ws_messages: Vec<WsMessage>,
    /// 错误信息（如有）。
    pub error: Option<String>,
}

impl Traffic {
    /// 创建一条「进行中」的流量记录。
    pub fn new(id: u64, protocol: &str, method: &str, url: &str, host: &str, client: &str) -> Self {
        Self {
            id,
            protocol: protocol.to_string(),
            method: method.to_string(),
            url: url.to_string(),
            host: host.to_string(),
            client: client.to_string(),
            status: None,
            req_headers: Vec::new(),
            res_headers: Vec::new(),
            start_time: now_ms(),
            duration_ms: None,
            rules: Vec::new(),
            req_body: None,
            res_body: None,
            req_body_size: 0,
            res_body_size: 0,
            res_body_truncated: false,
            ws_messages: Vec::new(),
            error: None,
        }
    }

    /// 追加一条 WebSocket 消息（超过 `max` 条后丢弃，避免无界增长）。
    pub fn push_ws(&mut self, msg: WsMessage, max: usize) {
        if self.ws_messages.len() < max {
            self.ws_messages.push(msg);
        }
    }

    /// 记录请求体预览（截断到 `limit` 字节）。
    pub fn set_req_body(&mut self, bytes: &[u8], limit: usize) {
        self.req_body_size = bytes.len() as u64;
        let take = bytes.len().min(limit);
        self.req_body = Some(String::from_utf8_lossy(&bytes[..take]).into_owned());
    }

    /// 记录响应体预览（截断到 `limit` 字节）。
    pub fn set_res_body(&mut self, bytes: &[u8], limit: usize) {
        self.res_body_size = bytes.len() as u64;
        let take = bytes.len().min(limit);
        self.res_body = Some(String::from_utf8_lossy(&bytes[..take]).into_owned());
        self.res_body_truncated = bytes.len() > limit;
    }

    /// 标记完成并计算耗时。
    pub fn finish(&mut self) {
        self.duration_ms = Some(now_ms().saturating_sub(self.start_time));
    }
}

/// 当前 Unix 毫秒时间戳。
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

struct Inner {
    order: VecDeque<u64>,
    map: HashMap<u64, Traffic>,
}

/// 内存抓包存储：保留最近 `capacity` 条，并向订阅者广播更新事件。
pub struct CaptureStore {
    inner: Mutex<Inner>,
    next_id: AtomicU64,
    capacity: usize,
    tx: broadcast::Sender<Traffic>,
}

impl CaptureStore {
    /// 创建容量为 `capacity` 的存储。
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            inner: Mutex::new(Inner {
                order: VecDeque::with_capacity(capacity),
                map: HashMap::with_capacity(capacity),
            }),
            next_id: AtomicU64::new(1),
            capacity: capacity.max(1),
            tx,
        }
    }

    /// 分配下一个流量 id。
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// 插入或更新一条记录（按 id），并广播事件。
    pub fn upsert(&self, traffic: Traffic) {
        {
            let mut inner = self.inner.lock().unwrap();
            if !inner.map.contains_key(&traffic.id) {
                inner.order.push_back(traffic.id);
                while inner.order.len() > self.capacity {
                    if let Some(old) = inner.order.pop_front() {
                        inner.map.remove(&old);
                    }
                }
            }
            inner.map.insert(traffic.id, traffic.clone());
        }
        // 没有订阅者时返回 Err，忽略即可。
        let _ = self.tx.send(traffic);
    }

    /// 返回最近的若干条记录（最新在前）。
    pub fn list(&self, limit: usize) -> Vec<Traffic> {
        let inner = self.inner.lock().unwrap();
        inner
            .order
            .iter()
            .rev()
            .take(limit)
            .filter_map(|id| inner.map.get(id).cloned())
            .collect()
    }

    /// 按 id 获取一条记录。
    pub fn get(&self, id: u64) -> Option<Traffic> {
        self.inner.lock().unwrap().map.get(&id).cloned()
    }

    /// 当前记录条数。
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().order.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 清空所有记录。
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.order.clear();
        inner.map.clear();
    }

    /// 订阅实时更新事件。
    pub fn subscribe(&self) -> broadcast::Receiver<Traffic> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_is_enforced_and_newest_first() {
        let store = CaptureStore::new(2);
        for _ in 0..3 {
            let id = store.next_id();
            store.upsert(Traffic::new(id, "http", "GET", "http://x/", "x", "c"));
        }
        assert_eq!(store.len(), 2);
        let list = store.list(10);
        assert_eq!(list.len(), 2);
        // 最新在前：id 3, 2
        assert_eq!(list[0].id, 3);
        assert_eq!(list[1].id, 2);
        // 最旧（id 1）已被淘汰
        assert!(store.get(1).is_none());
    }

    #[test]
    fn upsert_updates_existing() {
        let store = CaptureStore::new(8);
        let id = store.next_id();
        let mut t = Traffic::new(id, "http", "GET", "http://x/", "x", "c");
        store.upsert(t.clone());
        t.status = Some(200);
        store.upsert(t);
        assert_eq!(store.len(), 1);
        assert_eq!(store.get(id).unwrap().status, Some(200));
    }
}
