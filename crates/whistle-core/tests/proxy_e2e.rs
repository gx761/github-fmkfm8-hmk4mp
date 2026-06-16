//! 端到端集成测试：经代理转发 + 规则改写，全部在本地回环上进行。

use std::sync::{Arc, RwLock};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use whistle_core::{CaptureStore, CertAuthority, RuleSet};

/// 极简上游：读完请求头后返回固定 200 响应。
async fn spawn_origin(body: &'static str) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await; // 读掉请求
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    addr
}

/// 启动代理，返回其监听地址与抓包存储。
async fn spawn_proxy(rules_text: &str) -> (std::net::SocketAddr, Arc<CaptureStore>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let store = Arc::new(CaptureStore::new(100));
    let rules = Arc::new(RwLock::new(RuleSet::parse(rules_text).unwrap()));
    let dir = std::env::temp_dir().join(format!(
        "whistle-rs-it-{}-{}",
        std::process::id(),
        addr.port()
    ));
    let ca = Arc::new(CertAuthority::load_or_generate(&dir).unwrap());
    let store2 = store.clone();
    tokio::spawn(async move {
        let _ = whistle_core::proxy::serve_listener(listener, store2, rules, ca, false, 512 * 1024)
            .await;
    });
    (addr, store)
}

/// 用裸 TCP 发送一个代理请求（absolute-form），返回完整响应文本。
async fn proxy_get(proxy: std::net::SocketAddr, url: &str, host: &str) -> String {
    let mut sock = tokio::net::TcpStream::connect(proxy).await.unwrap();
    let req = format!("GET {url} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    sock.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    sock.read_to_string(&mut out).await.unwrap();
    out
}

#[tokio::test]
async fn host_rule_forwards_to_origin() {
    let origin = spawn_origin("HELLO-ORIGIN").await;
    let (proxy, store) =
        spawn_proxy(&format!("demo.local host://127.0.0.1:{}", origin.port())).await;

    let resp = proxy_get(proxy, "http://demo.local/path", "demo.local").await;
    assert!(resp.starts_with("HTTP/1.1 200"), "resp = {resp}");
    assert!(resp.contains("HELLO-ORIGIN"), "resp = {resp}");

    // 抓包应记录这条 http 流量且命中规则。
    let list = store.list(10);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].status, Some(200));
    assert_eq!(list[0].protocol, "http");
    assert!(list[0].rules.iter().any(|r| r.contains("host://")));
    // 响应体已被抓取（已知长度且不超限）。
    assert_eq!(list[0].res_body.as_deref(), Some("HELLO-ORIGIN"));
}

#[tokio::test]
async fn status_code_rule_mocks_without_upstream() {
    let (proxy, _store) = spawn_proxy("mock.local/teapot statusCode://418").await;
    let resp = proxy_get(proxy, "http://mock.local/teapot", "mock.local").await;
    assert!(resp.starts_with("HTTP/1.1 418"), "resp = {resp}");
}

#[tokio::test]
async fn res_body_rewrite_applies() {
    let origin = spawn_origin("ORIGINAL").await;
    let (proxy, _) = spawn_proxy(&format!(
        "body.local host://127.0.0.1:{} resReplace://ORIGINAL=PATCHED htmlAppend://!",
        origin.port()
    ))
    .await;
    let resp = proxy_get(proxy, "http://body.local/", "body.local").await;
    assert!(resp.contains("PATCHED!"), "resp = {resp}");
    assert!(!resp.contains("ORIGINAL"), "resp = {resp}");
}
