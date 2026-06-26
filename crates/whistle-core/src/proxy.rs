//! 代理服务：接入、规则求值、转发、CONNECT 隧道 / HTTPS 中间人。
//!
//! - 明文 HTTP：解析代理请求（absolute-form），按规则 mock 或转发上游；
//! - CONNECT + `decrypt_https=true`：动态签发证书做中间人，解密后逐请求按
//!   HTTPS 转发并抓包；`=false` 时退化为盲隧道（TCP 双向转发）。

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::client::conn::http1::SendRequest;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tracing::{debug, info, warn};

use whistle_capture::{CaptureStore, Header, Traffic};
use whistle_rules::{last_value, MatchInput, Operation, RuleSet};
use whistle_tls::CertAuthority;

use crate::Config;
use whistle_inspectors::{self as apply, RequestAction, ResBody};

/// 共享的请求上下文（跨连接克隆 Arc）。
#[derive(Clone)]
struct Ctx {
    store: Arc<CaptureStore>,
    rules: Arc<RwLock<RuleSet>>,
    ca: Arc<CertAuthority>,
    decrypt_https: bool,
    /// 是否解密全部 HTTPS（运行时可由 Web UI 切换）。false 时只解密命中规则的 host。
    intercept_all: Arc<AtomicBool>,
    body_limit: usize,
    plugins: Arc<HashMap<String, String>>,
    /// 管理界面路由：用浏览器「直接」访问代理端口（非代理请求）时返回看板。
    /// 这样代理与看板共用一个端口，和 whistle 一致。
    ui_router: Option<axum::Router>,
}

impl Ctx {
    /// 决定是否对某 host 做 HTTPS 中间人解密：需具备解密能力，且（全局拦截开启
    /// 或存在引用该 host 的规则）。否则盲隧道直通，保证未装根证书时站点仍可访问。
    fn should_mitm(&self, host: &str) -> bool {
        if !self.decrypt_https {
            return false;
        }
        if self.intercept_all.load(Ordering::Relaxed) {
            return true;
        }
        self.rules.read().unwrap().intercepts_host(host)
    }
}

/// 监听代理端口并处理连接，直到收到 Ctrl-C。
pub async fn serve(
    config: Config,
    store: Arc<CaptureStore>,
    rules: Arc<RwLock<RuleSet>>,
    ca: Arc<CertAuthority>,
    intercept_all: Arc<AtomicBool>,
    ui_router: Option<axum::Router>,
) -> crate::Result<()> {
    let addr = config.bind_addr();
    let listener = TcpListener::bind(&addr).await?;
    info!(
        %addr,
        decrypt_https = config.decrypt_https,
        intercept_all_https = config.intercept_all_https,
        "whistle-rs 代理已启动（浏览器直接访问本端口可打开看板）"
    );
    serve_listener(
        listener,
        store,
        rules,
        ca,
        config.decrypt_https,
        intercept_all,
        config.capture_body_limit,
        Arc::new(config.plugins.clone()),
        ui_router,
    )
    .await
}

/// 在给定监听器上处理连接（便于测试注入端口）。直到收到 Ctrl-C 返回。
#[allow(clippy::too_many_arguments)]
pub async fn serve_listener(
    listener: TcpListener,
    store: Arc<CaptureStore>,
    rules: Arc<RwLock<RuleSet>>,
    ca: Arc<CertAuthority>,
    decrypt_https: bool,
    intercept_all: Arc<AtomicBool>,
    body_limit: usize,
    plugins: Arc<HashMap<String, String>>,
    ui_router: Option<axum::Router>,
) -> crate::Result<()> {
    let ctx = Ctx {
        store,
        rules,
        ca,
        decrypt_https,
        intercept_all,
        body_limit,
        plugins,
        ui_router,
    };

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
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    // 同端口区分 SOCKS5（首字节 0x05）与 HTTP 代理请求。
                    let mut first = [0u8; 1];
                    if matches!(stream.peek(&mut first).await, Ok(1) if first[0] == 0x05) {
                        handle_socks5(stream, peer, ctx).await;
                        return;
                    }
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |req| handle(req, peer, ctx.clone()));
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

/// 顶层请求分发：CONNECT 走隧道/MITM，其余走 HTTP 转发。
async fn handle(
    req: Request<Incoming>,
    peer: SocketAddr,
    ctx: Ctx,
) -> Result<Response<ResBody>, Infallible> {
    if req.method() == Method::CONNECT {
        Ok(handle_connect(req, peer, ctx))
    } else if req.uri().host().is_none() {
        // 非代理（origin-form）请求：浏览器直接访问了代理端口 → 返回管理界面看板。
        // 代理请求一定是 absolute-form（带 host），据此区分。
        match ctx.ui_router.clone() {
            Some(router) => Ok(serve_ui(router, req).await),
            None => Ok(handle_http(req, peer, ctx).await),
        }
    } else {
        Ok(handle_http(req, peer, ctx).await)
    }
}

/// 把一个「直接」HTTP 请求交给管理界面 axum 路由处理，并把响应桥接回代理的 `ResBody`。
///
/// 管理界面响应均为有限大小（HTML/JSON/静态资源），故整体收集为字节再封装，
/// 以规避 axum `Body`（非 `Sync`）与代理 `ResBody`（`Sync` BoxBody）的类型差异。
async fn serve_ui(router: axum::Router, req: Request<Incoming>) -> Response<ResBody> {
    use tower::ServiceExt;
    let axum_req = req.map(axum::body::Body::new);
    // axum Router 的 Service 错误类型为 Infallible。
    let resp = match router.oneshot(axum_req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let (parts, body) = resp.into_parts();
    let bytes = body
        .collect()
        .await
        .map(|c| c.to_bytes().to_vec())
        .unwrap_or_default();
    Response::from_parts(parts, apply::full_body(bytes))
}

/// 明文 HTTP 转发（含规则求值与改写）。
async fn handle_http(req: Request<Incoming>, peer: SocketAddr, ctx: Ctx) -> Response<ResBody> {
    let store = &ctx.store;
    let id = store.next_id();
    let method = req.method().to_string();
    let uri = req.uri().clone();
    let url = uri.to_string();
    let host = uri.host().unwrap_or("").to_string();
    let port = uri.port_u16().unwrap_or(80);
    let path = uri.path().to_string();

    let mut traffic = Traffic::new(id, "http", &method, &url, &host, &peer.to_string());
    traffic.req_headers = collect_headers(req.headers());

    if host.is_empty() {
        return finish_error(
            store,
            &mut traffic,
            StatusCode::BAD_REQUEST,
            "缺少目标主机（请将本程序设置为 HTTP 代理后再访问）",
        );
    }

    let ops = eval_rules(&ctx, "http", &host, &path, &mut traffic);

    // plugin://name：命中且已配置则交由插件应答。
    if let Some(addr) = plugin_target(&ctx, &ops) {
        return run_plugin(req, &url, &addr, &ops, traffic, store, ctx.body_limit).await;
    }

    // tpl/xtpl：JSONP 模板 mock。
    if let Some(tpl) = last_value(&ops, "tpl").or_else(|| last_value(&ops, "xtpl")) {
        let cb = tpl_callback(uri.query().unwrap_or(""));
        return finish_mock(store, traffic, apply::serve_tpl(tpl, &cb));
    }

    let (up_host, up_port) = match apply::request_action(&ops, &host, port) {
        RequestAction::Mock(resp) => return finish_mock(store, traffic, resp),
        RequestAction::Forward { host, port } => (host, port),
    };

    if is_ws_upgrade(req.headers()) {
        traffic.protocol = "ws".to_string();
        return handle_ws(req, &up_host, up_port, None, traffic, ctx.store.clone()).await;
    }

    // 上游路由：socks://（隧道，origin-form）> proxy://（HTTP 代理，absolute-form）> 直连。
    let (sender, keep_absolute) = if let Some((sh, sp)) = apply::upstream_socks(&ops) {
        match socks5_connect(&format!("{sh}:{sp}"), &up_host, up_port).await {
            Ok(tcp) => (http1_client(tcp).await, false),
            Err(e) => return finish_error(store, &mut traffic, StatusCode::BAD_GATEWAY, &e),
        }
    } else if let Some((ph, pp)) = apply::upstream_proxy(&ops) {
        (connect_plain(&ph, pp).await, true)
    } else {
        (connect_plain(&up_host, up_port).await, false)
    };
    let sender = match sender {
        Ok(s) => s,
        Err(e) => return finish_error(store, &mut traffic, StatusCode::BAD_GATEWAY, &e),
    };
    send_and_capture(
        sender,
        req,
        &ops,
        traffic,
        store,
        keep_absolute,
        ctx.body_limit,
    )
    .await
}

/// CONNECT：MITM 解密或盲隧道。
fn handle_connect(req: Request<Incoming>, peer: SocketAddr, ctx: Ctx) -> Response<ResBody> {
    let authority = req
        .uri()
        .authority()
        .map(|a| a.to_string())
        .unwrap_or_else(|| req.uri().to_string());
    let (host, port) = split_authority(&authority, 443);

    if ctx.should_mitm(&host) {
        // 中间人：升级后用动态证书与客户端建立 TLS，再逐请求转发。
        tokio::spawn(async move {
            match hyper::upgrade::on(req).await {
                Ok(upgraded) => serve_mitm(TokioIo::new(upgraded), host, port, peer, ctx).await,
                Err(e) => debug!(%e, "CONNECT upgrade 失败"),
            }
        });
        return ok_200();
    }

    // 盲隧道（不解密）。
    let store = ctx.store.clone();
    let id = store.next_id();
    let mut traffic = Traffic::new(
        id,
        "tunnel",
        "CONNECT",
        &authority,
        &host,
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
                        if let Err(e) =
                            tokio::io::copy_bidirectional(&mut client_io, &mut upstream).await
                        {
                            debug!(%e, "隧道传输错误");
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
    ok_200()
}

/// 处理本机 SOCKS5 入站连接（仅 no-auth + CONNECT）。
///
/// 握手后建立到目标的隧道：若为 TLS 且开启解密则做中间人（复用 [`serve_mitm`]），
/// 否则盲隧道转发（明文 HTTP-over-SOCKS 暂只隧道、不抓取）。
async fn handle_socks5(mut stream: TcpStream, peer: SocketAddr, ctx: Ctx) {
    // 1) 方法协商：VER, NMETHODS, METHODS...
    let mut head = [0u8; 2];
    if stream.read_exact(&mut head).await.is_err() || head[0] != 0x05 {
        return;
    }
    let mut methods = vec![0u8; head[1] as usize];
    if stream.read_exact(&mut methods).await.is_err() {
        return;
    }
    // 选择 no-auth(0x00)。
    if stream.write_all(&[0x05, 0x00]).await.is_err() {
        return;
    }

    // 2) 请求：VER, CMD, RSV, ATYP, ADDR, PORT。
    let mut req = [0u8; 4];
    if stream.read_exact(&mut req).await.is_err() || req[0] != 0x05 {
        return;
    }
    let (cmd, atyp) = (req[1], req[3]);
    let host = match atyp {
        0x01 => {
            let mut a = [0u8; 4];
            if stream.read_exact(&mut a).await.is_err() {
                return;
            }
            std::net::Ipv4Addr::from(a).to_string()
        }
        0x03 => {
            let mut l = [0u8; 1];
            if stream.read_exact(&mut l).await.is_err() {
                return;
            }
            let mut d = vec![0u8; l[0] as usize];
            if stream.read_exact(&mut d).await.is_err() {
                return;
            }
            String::from_utf8_lossy(&d).into_owned()
        }
        0x04 => {
            let mut a = [0u8; 16];
            if stream.read_exact(&mut a).await.is_err() {
                return;
            }
            std::net::Ipv6Addr::from(a).to_string()
        }
        _ => {
            let _ = stream
                .write_all(&[0x05, 0x08, 0, 0x01, 0, 0, 0, 0, 0, 0])
                .await;
            return;
        }
    };
    let mut pbuf = [0u8; 2];
    if stream.read_exact(&mut pbuf).await.is_err() {
        return;
    }
    let port = u16::from_be_bytes(pbuf);

    if cmd != 0x01 {
        // 仅支持 CONNECT。
        let _ = stream
            .write_all(&[0x05, 0x07, 0, 0x01, 0, 0, 0, 0, 0, 0])
            .await;
        return;
    }
    // 成功应答（BND.ADDR/PORT 填 0）。
    if stream
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
        .is_err()
    {
        return;
    }

    // 3) 探测首字节决定 MITM(0x16=TLS) 还是盲隧道。
    let mut fb = [0u8; 1];
    let is_tls = matches!(stream.peek(&mut fb).await, Ok(1) if fb[0] == 0x16);
    if is_tls && ctx.should_mitm(&host) {
        serve_mitm(stream, host, port, peer, ctx).await;
        return;
    }

    // 盲隧道。
    let store = ctx.store.clone();
    let id = store.next_id();
    let mut traffic = Traffic::new(
        id,
        "tunnel",
        "SOCKS5",
        &format!("{host}:{port}"),
        &host,
        &peer.to_string(),
    );
    store.upsert(traffic.clone());
    match TcpStream::connect((host.as_str(), port)).await {
        Ok(mut upstream) => {
            traffic.status = Some(200);
            if let Err(e) = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await {
                debug!(%e, "SOCKS5 隧道传输错误");
            }
        }
        Err(e) => traffic.error = Some(format!("连接上游失败: {e}")),
    }
    traffic.finish();
    store.upsert(traffic);
}

/// 在给定（已建立的）客户端连接上做 TLS 中间人，并逐 HTTP 请求转发。
/// `io` 可以是 CONNECT 升级后的连接或 SOCKS5 隧道连接。
async fn serve_mitm<S>(io: S, host: String, port: u16, peer: SocketAddr, ctx: Ctx)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let server_cfg = match ctx.ca.server_config_for(&host) {
        Ok(c) => c,
        Err(e) => {
            warn!(%host, %e, "签发证书失败");
            return;
        }
    };
    let acceptor = TlsAcceptor::from(server_cfg);
    let tls = match acceptor.accept(io).await {
        Ok(t) => t,
        Err(e) => {
            debug!(%host, %e, "对客户端 TLS 握手失败");
            return;
        }
    };
    // 读取 ALPN 协商结果，决定以 h2 还是 http/1.1 服务客户端。
    let is_h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2".as_slice());

    let host = Arc::new(host);
    let service = service_fn(move |req| {
        let ctx = ctx.clone();
        let host = host.clone();
        async move { Ok::<_, Infallible>(handle_https(req, &host, port, peer, ctx).await) }
    });
    let io = TokioIo::new(tls);
    if is_h2 {
        // HTTP/2：上游仍走 http/1.1（在 handle_https 内连接），此处仅对客户端用 h2。
        if let Err(e) =
            hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .serve_connection(io, service)
                .await
        {
            debug!(%e, "MITM(h2) 连接结束");
        }
    } else if let Err(e) = hyper::server::conn::http1::Builder::new()
        .serve_connection(io, service)
        .with_upgrades()
        .await
    {
        debug!(%e, "MITM 连接结束");
    }
}

/// 解密后的单条 HTTPS 请求处理（转发到真实上游，over TLS）。
async fn handle_https(
    req: Request<Incoming>,
    host: &str,
    port: u16,
    peer: SocketAddr,
    ctx: Ctx,
) -> Response<ResBody> {
    let store = &ctx.store;
    let id = store.next_id();
    let method = req.method().to_string();
    let path_q = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_string();
    let path = req.uri().path().to_string();
    let url = format!("https://{host}{path_q}");

    let mut traffic = Traffic::new(id, "https", &method, &url, host, &peer.to_string());
    traffic.req_headers = collect_headers(req.headers());

    let ops = eval_rules(&ctx, "https", host, &path, &mut traffic);

    // plugin://name：命中且已配置则交由插件应答。
    if let Some(addr) = plugin_target(&ctx, &ops) {
        return run_plugin(req, &url, &addr, &ops, traffic, &ctx.store, ctx.body_limit).await;
    }

    // tpl/xtpl：JSONP 模板 mock。
    if let Some(tpl) = last_value(&ops, "tpl").or_else(|| last_value(&ops, "xtpl")) {
        let cb = tpl_callback(req.uri().query().unwrap_or(""));
        return finish_mock(store, traffic, apply::serve_tpl(tpl, &cb));
    }

    let (up_host, up_port) = match apply::request_action(&ops, host, port) {
        RequestAction::Mock(resp) => return finish_mock(store, traffic, resp),
        RequestAction::Forward { host, port } => (host, port),
    };

    if is_ws_upgrade(req.headers()) {
        traffic.protocol = "wss".to_string();
        return handle_ws(
            req,
            &up_host,
            up_port,
            Some(host.to_string()),
            traffic,
            ctx.store.clone(),
        )
        .await;
    }

    // 上游路由：socks://（SOCKS 隧道后 TLS）> proxy://（CONNECT 后 TLS）> 直连 TLS。
    let sender = if let Some((sh, sp)) = apply::upstream_socks(&ops) {
        match socks5_connect(&format!("{sh}:{sp}"), &up_host, up_port).await {
            Ok(tcp) => tls_client(tcp, host).await,
            Err(e) => Err(e),
        }
    } else if let Some((ph, pp)) = apply::upstream_proxy(&ops) {
        connect_tls_via_proxy(&ph, pp, &up_host, up_port, host).await
    } else {
        connect_tls(&up_host, up_port, host).await
    };
    let sender = match sender {
        Ok(s) => s,
        Err(e) => return finish_error(store, &mut traffic, StatusCode::BAD_GATEWAY, &e),
    };
    send_and_capture(sender, req, &ops, traffic, store, false, ctx.body_limit).await
}

/// 是否为 WebSocket 升级请求。
fn is_ws_upgrade(h: &hyper::HeaderMap) -> bool {
    let upgrade = h
        .get(hyper::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.eq_ignore_ascii_case("websocket"));
    let conn = h
        .get(hyper::header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.to_ascii_lowercase().contains("upgrade"));
    upgrade && conn
}

/// WebSocket 转发：转发握手到上游，101 后双向中继升级后的连接。
/// `sni=Some(host)` 表示走 TLS（wss），`None` 为明文（ws）。
async fn handle_ws(
    mut req: Request<Incoming>,
    up_host: &str,
    up_port: u16,
    sni: Option<String>,
    mut traffic: Traffic,
    store: Arc<CaptureStore>,
) -> Response<ResBody> {
    let mut sender = match &sni {
        Some(host) => connect_tls(up_host, up_port, host).await,
        None => connect_plain(up_host, up_port).await,
    };
    let sender = match &mut sender {
        Ok(s) => s,
        Err(e) => return finish_error(&store, &mut traffic, StatusCode::BAD_GATEWAY, e),
    };

    // 取客户端侧的升级 future（在消费 req 之前）。
    let client_upgrade = hyper::upgrade::on(&mut req);

    // 构造上游握手请求：origin-form，保留 Upgrade/Connection/Sec-WebSocket-* 头。
    let (mut parts, _body) = req.into_parts();
    let pq = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_string();
    parts.uri = pq.parse().unwrap_or_else(|_| "/".parse().unwrap());
    let upstream_req = Request::from_parts(parts, apply::empty_body());

    let mut upresp = match sender.send_request(upstream_req).await {
        Ok(r) => r,
        Err(e) => {
            return finish_error(
                &store,
                &mut traffic,
                StatusCode::BAD_GATEWAY,
                &format!("上游 WS 握手失败: {e}"),
            )
        }
    };

    // 上游未切换协议：原样返回。
    if upresp.status() != StatusCode::SWITCHING_PROTOCOLS {
        let (rparts, body) = upresp.into_parts();
        traffic.status = Some(rparts.status.as_u16());
        traffic.res_headers = collect_headers(&rparts.headers);
        traffic.finish();
        store.upsert(traffic);
        return Response::from_parts(rparts, body.map_err(Into::into).boxed());
    }

    let upstream_upgrade = hyper::upgrade::on(&mut upresp);

    // 回给客户端的 101 响应：复制上游响应头（含 Sec-WebSocket-Accept）。
    let mut client_resp = Response::new(apply::empty_body());
    *client_resp.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    *client_resp.headers_mut() = upresp.headers().clone();

    traffic.status = Some(101);
    traffic.res_headers = collect_headers(upresp.headers());
    store.upsert(traffic.clone());

    // 升级完成后双向中继，并做帧级抓取。
    let store2 = store.clone();
    tokio::spawn(async move {
        match tokio::join!(client_upgrade, upstream_upgrade) {
            (Ok(client), Ok(upstream)) => {
                let (cr, cw) = tokio::io::split(TokioIo::new(client));
                let (ur, uw) = tokio::io::split(TokioIo::new(upstream));
                let traffic = std::sync::Arc::new(std::sync::Mutex::new(traffic));
                // send: 客户端→上游；recv: 上游→客户端。
                let send = tokio::spawn(crate::ws::relay(
                    cr,
                    uw,
                    "send",
                    traffic.clone(),
                    store2.clone(),
                ));
                let recv = tokio::spawn(crate::ws::relay(
                    ur,
                    cw,
                    "recv",
                    traffic.clone(),
                    store2.clone(),
                ));
                let _ = tokio::join!(send, recv);
                let mut t = traffic.lock().unwrap();
                t.finish();
                store2.upsert(t.clone());
            }
            (c, u) => {
                traffic.error = Some(format!(
                    "WS 升级失败: client={:?} upstream={:?}",
                    c.err(),
                    u.err()
                ));
                traffic.finish();
                store2.upsert(traffic);
            }
        }
    });

    client_resp
}

/// 求值规则并写入 traffic.rules。
fn eval_rules(
    ctx: &Ctx,
    scheme: &str,
    host: &str,
    path: &str,
    traffic: &mut Traffic,
) -> Vec<Operation> {
    let input = MatchInput::new(scheme, host, path).with_method(&traffic.method);
    let ops = ctx.rules.read().unwrap().match_request(&input);
    traffic.rules = ops.iter().map(|o| o.raw.clone()).collect();
    ctx.store.upsert(traffic.clone());
    ops
}

/// 在已建立的流上完成 HTTP/1 握手，返回 sender（驱动连接，支持升级）。
async fn http1_client<S>(io: S) -> Result<SendRequest<ResBody>, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|e| format!("上游握手失败: {e}"))?;
    tokio::spawn(async move {
        // with_upgrades 以支持 WebSocket 等协议升级。
        if let Err(e) = conn.with_upgrades().await {
            debug!(%e, "上游连接结束");
        }
    });
    Ok(sender)
}

/// 在已建立的流上做 TLS（SNI=sni）后完成 HTTP/1 握手。
async fn tls_client<S>(io: S, sni: &str) -> Result<SendRequest<ResBody>, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let connector = TlsConnector::from(whistle_tls::client_config());
    let server_name =
        ServerName::try_from(sni.to_string()).map_err(|e| format!("非法 SNI: {e}"))?;
    let tls = connector
        .connect(server_name, io)
        .await
        .map_err(|e| format!("上游 TLS 握手失败: {e}"))?;
    http1_client(tls).await
}

/// 建立到上游的明文 HTTP/1 连接。
async fn connect_plain(host: &str, port: u16) -> Result<SendRequest<ResBody>, String> {
    let stream = TcpStream::connect((host, port))
        .await
        .map_err(|e| format!("连接上游失败: {e}"))?;
    http1_client(stream).await
}

/// 建立到上游的 TLS HTTP/1 连接（SNI = 原始 host）。
async fn connect_tls(host: &str, port: u16, sni: &str) -> Result<SendRequest<ResBody>, String> {
    let tcp = TcpStream::connect((host, port))
        .await
        .map_err(|e| format!("连接上游失败: {e}"))?;
    tls_client(tcp, sni).await
}

/// 经上游 HTTP 代理建立到目标的 TLS 连接：先对上游代理发 CONNECT 打隧道，
/// 再在隧道上做 TLS（SNI = 原始 host）。用于 `proxy://` + HTTPS 的级联场景。
async fn connect_tls_via_proxy(
    proxy_host: &str,
    proxy_port: u16,
    target_host: &str,
    target_port: u16,
    sni: &str,
) -> Result<SendRequest<ResBody>, String> {
    let mut tcp = TcpStream::connect((proxy_host, proxy_port))
        .await
        .map_err(|e| format!("连接上游代理失败: {e}"))?;
    let connect_req = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\n\r\n"
    );
    tcp.write_all(connect_req.as_bytes())
        .await
        .map_err(|e| format!("发送 CONNECT 失败: {e}"))?;

    // 读取 CONNECT 响应头（到 \r\n\r\n 为止；隧道建立前服务端不会先发数据）。
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        let n = tcp
            .read(&mut byte)
            .await
            .map_err(|e| format!("读取 CONNECT 响应失败: {e}"))?;
        if n == 0 {
            break;
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
        if buf.len() > 8192 {
            return Err("CONNECT 响应过大".to_string());
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let ok = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .map(|c| c.starts_with('2'))
        .unwrap_or(false);
    if !ok {
        return Err(format!(
            "上游代理 CONNECT 失败: {}",
            head.lines().next().unwrap_or("")
        ));
    }
    tls_client(tcp, sni).await
}

/// 通过上游 SOCKS5 代理（no-auth）连到目标，返回隧道 TcpStream（域名由上游解析）。
async fn socks5_connect(
    socks_addr: &str,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream, String> {
    let mut tcp = TcpStream::connect(socks_addr)
        .await
        .map_err(|e| format!("连接上游 SOCKS 失败: {e}"))?;
    // 方法协商：VER=5, NMETHODS=1, METHOD=0(no-auth)。
    tcp.write_all(&[0x05, 0x01, 0x00])
        .await
        .map_err(|e| e.to_string())?;
    let mut sel = [0u8; 2];
    tcp.read_exact(&mut sel).await.map_err(|e| e.to_string())?;
    if sel[0] != 0x05 || sel[1] != 0x00 {
        return Err("上游 SOCKS 不支持 no-auth".to_string());
    }
    // CONNECT 请求（域名形式，由上游解析）。
    let host_bytes = target_host.as_bytes();
    if host_bytes.len() > 255 {
        return Err("SOCKS 目标域名过长".to_string());
    }
    let mut req = vec![0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8];
    req.extend_from_slice(host_bytes);
    req.extend_from_slice(&target_port.to_be_bytes());
    tcp.write_all(&req).await.map_err(|e| e.to_string())?;
    // 应答：VER, REP, RSV, ATYP, BND.ADDR, BND.PORT。
    let mut head = [0u8; 4];
    tcp.read_exact(&mut head).await.map_err(|e| e.to_string())?;
    if head[1] != 0x00 {
        return Err(format!("上游 SOCKS CONNECT 失败 (REP={})", head[1]));
    }
    let skip = match head[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut l = [0u8; 1];
            tcp.read_exact(&mut l).await.map_err(|e| e.to_string())?;
            l[0] as usize
        }
        _ => return Err("上游 SOCKS 应答 ATYP 非法".to_string()),
    };
    let mut rest = vec![0u8; skip + 2];
    tcp.read_exact(&mut rest).await.map_err(|e| e.to_string())?;
    Ok(tcp)
}

/// 转发请求到上游、应用请求/响应改写（含 body）、记录抓包。
///
/// `keep_absolute=true` 时保留原始 absolute-form URI（用于经上游 HTTP 代理转发），
/// 否则改写为 origin-form 并合并 urlParams。
async fn send_and_capture(
    mut sender: SendRequest<ResBody>,
    req: Request<Incoming>,
    ops: &[Operation],
    mut traffic: Traffic,
    store: &CaptureStore,
    keep_absolute: bool,
    body_limit: usize,
) -> Response<ResBody> {
    let (mut parts, body) = req.into_parts();
    if !keep_absolute {
        let pq = parts
            .uri
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/")
            .to_string();
        let pq = apply::merge_url_params(&pq, ops);
        let pq = apply::rewrite_path(&pq, ops);
        parts.uri = pq.parse().unwrap_or_else(|_| "/".parse().unwrap());
    }
    remove_hop_headers(&mut parts.headers);
    apply::apply_request_headers(&mut parts.headers, ops);
    apply::override_method(ops, &mut parts);

    // 请求阶段延迟。
    if let Some(d) = apply::req_delay(ops) {
        tokio::time::sleep(d).await;
    }

    // 请求体：需改写或可抓包（已知长度且不超限）时缓冲，否则流式透传。
    let req_rewrite = apply::has_req_body_rewrite(ops);
    let req_body: ResBody = if req_rewrite || body_within_limit(&parts.headers, body_limit) {
        match body.collect().await {
            Ok(c) => {
                let bytes = c.to_bytes();
                let out = if req_rewrite {
                    let new = apply::rewrite_req_body(&bytes, ops);
                    apply::set_content_length(&mut parts.headers, new.len());
                    new
                } else {
                    bytes.to_vec()
                };
                traffic.set_req_body(&out, body_limit);
                if let Some(d) = apply::req_speed_delay(ops, out.len()) {
                    tokio::time::sleep(d).await;
                }
                apply::full_body(out)
            }
            Err(e) => {
                return finish_error(
                    store,
                    &mut traffic,
                    StatusCode::BAD_GATEWAY,
                    &format!("读取请求体失败: {e}"),
                )
            }
        }
    } else {
        body.map_err(Into::into).boxed()
    };

    let upstream_req = Request::from_parts(parts, req_body);
    match sender.send_request(upstream_req).await {
        Ok(resp) => {
            let (mut rparts, body) = resp.into_parts();
            apply::apply_response(&mut rparts, ops);
            apply::override_status(ops, &mut rparts.status);

            if let Some(d) = apply::res_delay(ops) {
                tokio::time::sleep(d).await;
            }

            // 响应体：需改写或可抓包（已知长度且不超限）时缓冲，否则流式透传。
            let res_rewrite = apply::has_res_body_rewrite(ops);
            let res_body: ResBody = if res_rewrite || body_within_limit(&rparts.headers, body_limit)
            {
                match body.collect().await {
                    Ok(c) => {
                        let bytes = c.to_bytes();
                        let out = if res_rewrite {
                            let new = apply::rewrite_res_body(&bytes, ops);
                            apply::set_content_length(&mut rparts.headers, new.len());
                            new
                        } else {
                            bytes.to_vec()
                        };
                        traffic.set_res_body(&out, body_limit);
                        if let Some(d) = apply::res_speed_delay(ops, out.len()) {
                            tokio::time::sleep(d).await;
                        }
                        apply::full_body(out)
                    }
                    Err(e) => {
                        return finish_error(
                            store,
                            &mut traffic,
                            StatusCode::BAD_GATEWAY,
                            &format!("读取响应体失败: {e}"),
                        )
                    }
                }
            } else {
                body.map_err(Into::into).boxed()
            };

            traffic.status = Some(rparts.status.as_u16());
            traffic.res_headers = collect_headers(&rparts.headers);
            traffic.finish();
            store.upsert(traffic);
            Response::from_parts(rparts, res_body)
        }
        Err(e) => finish_error(
            store,
            &mut traffic,
            StatusCode::BAD_GATEWAY,
            &format!("上游请求失败: {e}"),
        ),
    }
}

/// 响应/请求是否有「已知且不超限」的正文长度（决定是否缓冲抓包）。
fn body_within_limit(headers: &hyper::HeaderMap, limit: usize) -> bool {
    headers
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .map(|n| n > 0 && n <= limit)
        .unwrap_or(false)
}

/// 记录 mock 响应并返回。
fn finish_mock(
    store: &CaptureStore,
    mut traffic: Traffic,
    resp: Response<ResBody>,
) -> Response<ResBody> {
    traffic.status = Some(resp.status().as_u16());
    traffic.res_headers = collect_headers(resp.headers());
    traffic.finish();
    store.upsert(traffic);
    resp
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
        .body(apply::text_body(msg))
        .expect("构造错误响应不应失败")
}

fn ok_200() -> Response<ResBody> {
    Response::builder()
        .status(StatusCode::OK)
        .body(apply::empty_body())
        .expect("构造 200 响应不应失败")
}

/// 拆分 `host:port`，缺省端口用 `default`。
fn split_authority(authority: &str, default: u16) -> (String, u16) {
    match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(default)),
        None => (authority.to_string(), default),
    }
}

/// 从 query 中取 JSONP 回调名（`callback`/`_callback`/`jsonpCallback`），无则空串。
fn tpl_callback(query: &str) -> String {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if matches!(k, "callback" | "_callback" | "jsonpCallback") {
                return v.to_string();
            }
        }
    }
    String::new()
}

/// 若命中 `plugin://name` 且该 name 已在配置中映射，返回插件地址。
fn plugin_target(ctx: &Ctx, ops: &[Operation]) -> Option<String> {
    let name = last_value(ops, "plugin")?;
    ctx.plugins.get(name).cloned()
}

/// 调用插件并据其返回构造响应（编程式 mock）。
async fn run_plugin(
    req: Request<Incoming>,
    url: &str,
    addr: &str,
    ops: &[Operation],
    mut traffic: Traffic,
    store: &CaptureStore,
    body_limit: usize,
) -> Response<ResBody> {
    let (parts, body) = req.into_parts();
    let headers = parts
        .headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    // plugin-vars://k=v&... → 透传给插件的变量。
    let vars = whistle_rules::all_values(ops, "plugin-vars")
        .iter()
        .flat_map(|v| v.split('&'))
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, val) = p.split_once('=').unwrap_or((p, ""));
            (k.to_string(), val.to_string())
        })
        .collect();
    let body_bytes = body
        .collect()
        .await
        .map(|c| c.to_bytes())
        .unwrap_or_default();
    let preq = whistle_plugin::PluginRequest {
        method: parts.method.to_string(),
        url: url.to_string(),
        headers,
        body: String::from_utf8_lossy(&body_bytes).into_owned(),
        vars,
    };

    match whistle_plugin::invoke(addr, &preq).await {
        Ok(presp) => {
            let status = presp.status.unwrap_or(200);
            let mut builder =
                Response::builder().status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK));
            for (k, v) in &presp.headers {
                builder = builder.header(k, v);
            }
            let resp = match builder.body(apply::full_body(presp.body.clone().into_bytes())) {
                Ok(r) => r,
                Err(_) => {
                    return finish_error(
                        store,
                        &mut traffic,
                        StatusCode::BAD_GATEWAY,
                        "插件响应头非法",
                    )
                }
            };
            traffic.status = Some(status);
            traffic.res_headers = collect_headers(resp.headers());
            traffic.set_res_body(presp.body.as_bytes(), body_limit);
            traffic.finish();
            store.upsert(traffic);
            resp
        }
        Err(e) => finish_error(
            store,
            &mut traffic,
            StatusCode::BAD_GATEWAY,
            &format!("插件调用失败: {e}"),
        ),
    }
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
