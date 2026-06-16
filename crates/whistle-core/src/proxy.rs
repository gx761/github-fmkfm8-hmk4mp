//! 代理服务：接入、转发、CONNECT 隧道。
//!
//! M1 范围：
//! - 明文 HTTP：解析代理请求（absolute-form），转发到上游并回写响应，记录抓包；
//! - CONNECT：建立盲隧道（TCP 双向转发），HTTPS 解密留待 M3。

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

use whistle_capture::{CaptureStore, Header, Traffic};

use crate::Config;

/// 统一的错误盒子，便于桥接不同 body 的错误类型。
type BoxError = Box<dyn std::error::Error + Send + Sync>;
/// 出站响应 body 类型。
type ResBody = BoxBody<Bytes, BoxError>;

/// 监听代理端口并处理连接，直到收到 Ctrl-C。
pub async fn serve(config: Config, store: Arc<CaptureStore>) -> crate::Result<()> {
    let addr = config.bind_addr();
    let listener = TcpListener::bind(&addr).await?;
    info!(%addr, "whistle-rs 代理已启动（HTTP 抓包可用，HTTPS 走盲隧道）");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("收到 Ctrl-C，正在退出");
                return Ok(());
            }
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(v) => v,
                    Err(e) => { warn!(%e, "accept 失败"); continue; }
                };
                let store = store.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |req| handle(req, peer, store.clone()));
                    if let Err(e) = hyper::server::conn::http1::Builder::new()
                        .preserve_header_case(true)
                        .serve_connection(io, service)
                        .with_upgrades()
                        .await
                    {
                        debug!(%e, "连接处理结束");
                    }
                });
            }
        }
    }
}

/// 顶层请求分发：CONNECT 走隧道，其余走 HTTP 转发。
async fn handle(
    req: Request<Incoming>,
    peer: SocketAddr,
    store: Arc<CaptureStore>,
) -> Result<Response<ResBody>, Infallible> {
    if req.method() == Method::CONNECT {
        Ok(handle_connect(req, peer, store))
    } else {
        Ok(handle_http(req, peer, store).await)
    }
}

/// 明文 HTTP 转发。
async fn handle_http(
    req: Request<Incoming>,
    peer: SocketAddr,
    store: Arc<CaptureStore>,
) -> Response<ResBody> {
    let id = store.next_id();
    let method = req.method().to_string();
    let uri = req.uri().clone();
    let url = uri.to_string();
    let host = uri.host().unwrap_or("").to_string();
    let port = uri.port_u16().unwrap_or(80);

    let mut traffic = Traffic::new(id, "http", &method, &url, &host, &peer.to_string());
    traffic.req_headers = collect_headers(req.headers());
    store.upsert(traffic.clone());

    if host.is_empty() {
        return finish_error(
            &store,
            &mut traffic,
            StatusCode::BAD_REQUEST,
            "缺少目标主机（请将本程序设置为 HTTP 代理后再访问）",
        );
    }

    // 连接上游并完成 HTTP/1 握手。
    let stream = match TcpStream::connect((host.as_str(), port)).await {
        Ok(s) => s,
        Err(e) => {
            return finish_error(
                &store,
                &mut traffic,
                StatusCode::BAD_GATEWAY,
                &format!("连接上游失败: {e}"),
            )
        }
    };
    let io = TokioIo::new(stream);
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(io).await {
        Ok(v) => v,
        Err(e) => {
            return finish_error(
                &store,
                &mut traffic,
                StatusCode::BAD_GATEWAY,
                &format!("上游握手失败: {e}"),
            )
        }
    };
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            debug!(%e, "上游连接结束");
        }
    });

    // 代理收到的是 absolute-form 目标，转发上游时改写为 origin-form。
    let (mut parts, body) = req.into_parts();
    let pq = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_string();
    parts.uri = pq.parse().unwrap_or_else(|_| "/".parse().unwrap());
    remove_hop_headers(&mut parts.headers);
    let upstream_req = Request::from_parts(parts, body);

    match sender.send_request(upstream_req).await {
        Ok(resp) => {
            traffic.status = Some(resp.status().as_u16());
            traffic.res_headers = collect_headers(resp.headers());
            traffic.finish();
            store.upsert(traffic);
            let (parts, body) = resp.into_parts();
            Response::from_parts(parts, body.map_err(Into::into).boxed())
        }
        Err(e) => finish_error(
            &store,
            &mut traffic,
            StatusCode::BAD_GATEWAY,
            &format!("上游请求失败: {e}"),
        ),
    }
}

/// CONNECT 盲隧道：先回 200，再在升级后做 TCP 双向转发。
fn handle_connect(
    req: Request<Incoming>,
    peer: SocketAddr,
    store: Arc<CaptureStore>,
) -> Response<ResBody> {
    let authority = req
        .uri()
        .authority()
        .map(|a| a.to_string())
        .unwrap_or_else(|| req.uri().to_string());
    let id = store.next_id();
    let mut traffic = Traffic::new(
        id,
        "tunnel",
        "CONNECT",
        &authority,
        host_of(&authority),
        &peer.to_string(),
    );
    traffic.req_headers = collect_headers(req.headers());
    store.upsert(traffic.clone());

    let target = authority.clone();
    tokio::spawn(async move {
        match hyper::upgrade::on(req).await {
            Ok(upgraded) => {
                let mut client_io = TokioIo::new(upgraded);
                match TcpStream::connect(&target).await {
                    Ok(mut upstream) => {
                        traffic.status = Some(200);
                        match tokio::io::copy_bidirectional(&mut client_io, &mut upstream).await {
                            Ok((up, down)) => debug!(target=%target, up, down, "隧道关闭"),
                            Err(e) => debug!(%e, "隧道传输错误"),
                        }
                    }
                    Err(e) => traffic.error = Some(format!("连接上游失败: {e}")),
                }
            }
            Err(e) => traffic.error = Some(format!("upgrade 失败: {e}")),
        }
        traffic.finish();
        store.upsert(traffic);
    });

    Response::builder()
        .status(StatusCode::OK)
        .body(empty_body())
        .expect("构造 200 响应不应失败")
}

/// 记录失败并返回错误响应。
fn finish_error(
    store: &CaptureStore,
    traffic: &mut Traffic,
    status: StatusCode,
    msg: &str,
) -> Response<ResBody> {
    warn!(id = traffic.id, status = status.as_u16(), msg, "请求失败");
    traffic.status = Some(status.as_u16());
    traffic.error = Some(msg.to_string());
    traffic.finish();
    store.upsert(traffic.clone());
    Response::builder()
        .status(status)
        .body(text_body(msg))
        .expect("构造错误响应不应失败")
}

fn host_of(authority: &str) -> &str {
    authority.split(':').next().unwrap_or(authority)
}

fn collect_headers(map: &hyper::HeaderMap) -> Vec<Header> {
    map.iter()
        .map(|(k, v)| Header {
            name: k.as_str().to_string(),
            value: String::from_utf8_lossy(v.as_bytes()).into_owned(),
        })
        .collect()
}

/// 移除逐跳（hop-by-hop）首部，避免在代理转发时泄漏或冲突。
fn remove_hop_headers(map: &mut hyper::HeaderMap) {
    use hyper::header::{
        CONNECTION, PROXY_AUTHENTICATE, PROXY_AUTHORIZATION, TE, TRAILER, TRANSFER_ENCODING,
        UPGRADE,
    };
    for h in [
        &CONNECTION,
        &PROXY_AUTHENTICATE,
        &PROXY_AUTHORIZATION,
        &TE,
        &TRAILER,
        &TRANSFER_ENCODING,
        &UPGRADE,
    ] {
        map.remove(h);
    }
    map.remove("keep-alive");
    map.remove("proxy-connection");
}

fn empty_body() -> ResBody {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

fn text_body(s: &str) -> ResBody {
    Full::new(Bytes::from(s.to_string()))
        .map_err(|never| match never {})
        .boxed()
}
