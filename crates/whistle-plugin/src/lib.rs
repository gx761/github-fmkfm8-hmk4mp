//! 插件协议与调度（插件即本地 HTTP 服务）。
//!
//! 对应 whistle 的 `lib/plugins`，但采用 whistle-rs 自有的简单契约（非 whistle npm
//! 插件的二进制兼容）：
//! - 规则 `plugin://<name>` 命中时，代理把请求序列化为 JSON，`POST` 到插件地址的
//!   `/handle`；
//! - 插件返回 JSON [`PluginResponse`]，代理据此直接产生响应（编程式 mock）。
//!
//! 插件地址在配置中以 `name → host:port` 映射给出（见 `Config::plugins`）。

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 发给插件的请求描述。
#[derive(Debug, Serialize)]
pub struct PluginRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// 来自规则 `plugin-vars://k=v&...` 的变量，透传给插件。
    pub vars: Vec<(String, String)>,
}

/// 插件返回的响应指令。
#[derive(Debug, Deserialize)]
pub struct PluginResponse {
    /// 响应状态码（缺省 200）。
    pub status: Option<u16>,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: String,
}

/// 调用插件：`POST http://<addr>/handle`，请求/响应均为 JSON。
pub async fn invoke(addr: &str, req: &PluginRequest) -> Result<PluginResponse, String> {
    let payload = serde_json::to_string(req).map_err(|e| e.to_string())?;
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|e| format!("连接插件失败: {e}"))?;
    let http = format!(
        "POST /handle HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    );
    stream
        .write_all(http.as_bytes())
        .await
        .map_err(|e| format!("发送插件请求失败: {e}"))?;
    stream.flush().await.ok();

    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .await
        .map_err(|e| format!("读取插件响应失败: {e}"))?;
    let pos = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("插件响应缺少头部分隔")?;
    serde_json::from_slice(&buf[pos + 4..]).map_err(|e| format!("解析插件响应失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_defaults() {
        let r: PluginResponse = serde_json::from_str(r#"{"body":"hi"}"#).unwrap();
        assert_eq!(r.status, None);
        assert!(r.headers.is_empty());
        assert_eq!(r.body, "hi");
    }

    #[test]
    fn request_serializes() {
        let req = PluginRequest {
            method: "GET".into(),
            url: "http://x/".into(),
            headers: vec![("a".into(), "b".into())],
            body: String::new(),
            vars: vec![],
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"method\":\"GET\""));
        assert!(s.contains("\"url\":\"http://x/\""));
    }
}
