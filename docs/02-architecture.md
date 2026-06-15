# 02 · 目标整体架构

## 2.1 分层视图

```
┌──────────────────────────────────────────────────────────────┐
│                     Web UI (浏览器前端)                         │
│        抓包列表 / 规则编辑 / Composer / 插件管理                  │
└───────────────▲───────────────────────────▲───────────────────┘
                │ HTTP/JSON + WebSocket(实时推送)
┌───────────────┴───────────────────────────┴───────────────────┐
│                 管理面 API 服务 (axum)  —— whistle-web           │
│   REST: 规则 CRUD / 抓包查询 / 设置 ; WS: 实时流量事件推送        │
└───────────────▲────────────────────────────────────────────────┘
                │ 内部事件总线 / 共享状态
┌───────────────┴────────────────────────────────────────────────┐
│                        代理内核 whistle-core                     │
│                                                                  │
│  入站监听 ──▶ 协议识别 ──▶ 规则匹配 ──▶ 请求改写 ──▶ 上游分发     │
│   (listener)   (HTTP/CONNECT/   (rule engine) (req inspectors)  │
│                 SOCKS/TLS-sniff)                                 │
│                                          ┌─────────────────────┐ │
│  事件上报 ◀── 响应改写 ◀── 上游响应 ◀────┤  upstream connector  │ │
│  (capture)    (res inspectors)           │ direct/proxy/socks   │ │
│                                          └─────────────────────┘ │
│  子系统: TLS/CA 管理 · WS 隧道 · TCP 隧道 · 插件调度 · 抓包存储   │
└──────────────────────────────────────────────────────────────────┘
```

## 2.2 进程模型

whistle-client（Electron 版）采用「代理服务独立进程 + UI 进程」的多进程模型。本项目首版采用**单进程多任务**：

- 一个 tokio 运行时承载代理内核与管理面 API（不同端口或同端口区分）。
- 代理端口（如 `8899`）：发往该端口且 Host 指向代理自身的请求被视为**内部管理请求**，路由到 Web UI / API；其余请求按代理流程处理（与 whistle 行为一致）。
- 后续可演进为「core 进程 + 控制进程」以提升隔离性，但非首版目标。

## 2.3 请求生命周期（数据流）

1. **接入（Listener）**：TCP 监听代理端口。读取首字节/首行判断：
   - `CONNECT host:port` → 进入隧道/HTTPS-MITM 分支；
   - 明文 HTTP 请求行 → HTTP 分支；
   - SOCKS5 握手字节 → SOCKS 分支；
   - TLS ClientHello（SNI sniff）→ 透明 HTTPS 分支。
2. **协议识别与升级**：识别 HTTP/1.1、HTTP/2（h2 / ALPN）、WebSocket upgrade。
3. **构造 RequestContext**：URL、方法、头、客户端信息、时间戳，分配唯一 `traffic_id`。
4. **规则匹配**：规则引擎对该请求求值，得到一组「生效协议 + 参数」（见 [04-rules-engine.md](04-rules-engine.md)）。
5. **请求侧 inspectors**：按生效规则改写请求（host 重定向、headers、body、mock 文件、延迟/限速、鉴权等）。可能直接产生本地响应（file/tpl/statusCode）而短路上游。
6. **上游连接（Connector）**：
   - 直连 / 上游 HTTP 代理（proxy）/ 上游 SOCKS（socks）/ PAC 决策；
   - 对 HTTPS 走 MITM：用动态签发的叶子证书与客户端建立 TLS，再与真实服务器建立 TLS（rustls）。
7. **响应侧 inspectors**：改写状态码、响应头、响应体（注入 html/js/css、替换、CORS 等）。
8. **回写客户端** 并 **零拷贝转发** 剩余流。
9. **抓包上报**：将请求/响应元数据与（按需）正文写入抓包存储，并通过 WebSocket 实时推送给 UI。

## 2.4 核心子系统

- **TLS/CA 管理**：启动时确保存在根 CA（首次生成）；按目标域名动态签发叶子证书并缓存；提供 CA 导出/安装引导。
- **WebSocket / TCP 隧道**：对升级后的连接做双向转发并仍可抓帧（WS 帧级、TCP 字节级）。
- **抓包存储**：环形缓冲 + 可选落盘；正文按大小阈值决定内存/临时文件，支持懒加载。
- **插件调度**：将匹配到 `plugin://` 的请求转发给插件（首版采用与 whistle 一致的「插件作为本地 HTTP 服务」模型，后续评估 WASM 方案）。
- **配置与状态**：规则文本、设置、CA、抓包索引；规则热更新（编辑即生效）。

## 2.5 Crate 拆分（workspace）

```
whistle-rs/                # Cargo workspace
├── crates/
│   ├── whistle-core/      # 代理内核：listener/连接/转发/隧道/MITM 调度
│   ├── whistle-rules/     # 规则 DSL：解析、匹配、求值
│   ├── whistle-tls/       # CA 生成、动态签发、rustls 集成
│   ├── whistle-proto/     # HTTP/1.1·H2·WS·SOCKS 协议处理
│   ├── whistle-capture/   # 抓包数据模型、存储、事件流
│   ├── whistle-inspectors/# 各规则协议的请求/响应改写实现
│   ├── whistle-plugin/    # 插件协议与调度
│   ├── whistle-web/       # axum 管理面 API + 静态 UI 托管 + WS 推送
│   └── whistle-cli/       # `w2r` 命令行入口（start/stop/ca/...）
└── docs/
```

详见 [03-module-mapping.md](03-module-mapping.md) 与 [05-tech-stack.md](05-tech-stack.md)。
