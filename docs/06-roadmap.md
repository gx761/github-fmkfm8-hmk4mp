# 06 · 路线图（分阶段里程碑）

按「先打通端到端最小链路，再横向铺协议、纵向补能力」推进。每个里程碑都应可运行、可演示、有测试。

## M0 · 工程脚手架（Foundation）— ✅ 已完成
- [x] 初始化 Cargo workspace 与 9 个 crate（见 [02-architecture.md](02-architecture.md#25-crate-拆分workspace)）。
- [x] 接入 `clap` CLI 骨架：`w2r start/stop/status/ca`。
- [x] 接入 `tracing` 日志、`rustfmt`、CI（fmt + clippy + 多平台 build + test）。
- **产出**：`w2r --help` 可运行；`cargo fmt/clippy/test` 全绿。

## M1 · HTTP 透传代理（Plain HTTP MVP）
- 监听代理端口，作为系统代理转发明文 HTTP 请求并返回响应。
- 构造 `TrafficCtx`、分配 `traffic_id`、记录请求/响应元数据到 `whistle-capture`（内存环形缓冲）。
- **产出**：浏览器设代理后可正常上网，内核记录到流量条目。

## M2 · 规则引擎 + P0 协议 — ✅ 已完成（核心）
- [x] `whistle-rules`：DSL 解析（正则/通配符/URL 前缀/域名+路径）、模式匹配、求值管线（含 11 项单测）。
- [x] 实现 P0 协议：`host`、`redirect`、`file`/`rawfile`、`statusCode`、`reqHeaders`/`resHeaders`、`reqType`/`resType`，附带 `ua`/`referer`。
- [x] 端到端验证：经代理对 host/redirect/file/statusCode/resHeaders 均行为正确。
- 待补：`xhost`/`xfile`/`tpl`、`reqCookies`/`resCookies`、`ignore`/`disable`/`enable` 等（后续补齐）。
- **产出**：可用文本规则（`--rules`）改写/Mock 真实请求，示例见 `examples/rules.txt`。

## M3 · HTTPS MITM — ✅ 已完成
- [x] `whistle-tls`：首启生成根 CA 并持久化（重启复用）；按 host 动态签发并缓存叶子证书；
  `w2r ca export` 导出根证书、`w2r ca path` 查看路径。
- [x] CONNECT + rustls 双向 TLS：对客户端用动态证书、对上游用 NoVerify 客户端，
  解密后逐请求应用规则、再加密转发；`--no-decrypt` 可退化为盲隧道。
- [x] 端到端验证：经代理访问本地 HTTPS origin，解密成功且 statusCode/resHeaders 规则生效。
- **产出**：`w2r ca export` 安装根证书后即可抓取/改写 HTTPS 流量。

## M4 · Web UI（最小可用）
- `whistle-web`：axum REST（规则 CRUD、抓包查询、设置）+ WebSocket 实时推送。
- 内嵌精简前端（或复用 whistle 前端资源）：抓包列表 + 单请求详情 + 规则编辑。
- 规则热更新（编辑即生效）。
- **产出**：浏览器打开管理界面即可看流量、改规则。

## M5 · WebSocket / TCP / 上游代理
- WS upgrade 与帧级抓取；TCP 隧道字节级抓取。
- 上游路由：`proxy`/`https-proxy`/`socks`/`pac`，本地 SOCKS5 入站。
- **产出**：覆盖 WS/TCP 抓包与级联代理场景。

## M6 · P1 协议横向铺开
- Body 改写与注入（`reqBody`/`resBody`/`*Replace`/`html*`/`js*`/`css*`/`style`/`*Script`）。
- 头/Cookie 细节（`headerReplace`/`referer`/`ua`/`forwardedFor`/`*Cors`/`auth`/`*Charset`）。
- 网络模拟（`reqDelay`/`resDelay`/`reqSpeed`/`resSpeed`）。
- 状态/重写（`replaceStatus`/`method`/`urlParams`/`pathReplace`/`locationHref`/`attachment`）。
- 规则注入（`reqRules`/`resRules`/`inherit`/`includeFilter`/`excludeFilter`/`skip`）。
- **产出**：覆盖绝大多数日常调试场景。

## M7 · 插件体系
- `whistle-plugin`：兼容「插件即本地 HTTP 服务」模型，支持 `plugin://`、`plugin-vars`、`pipe`。
- **产出**：可对接现有 whistle 插件（按 HTTP 契约）。

## M8 · HTTP/2 与 P2 协议、打磨
- HTTP/2 MITM（多路复用 + ALPN）。
- P2 协议：`reqWrite*`/`resWrite*`/`*Merge`/`responseFor`/`cipher`/`sniCallback`/`tunnel`/`trailers`/`cache`/`log` 等。
- Composer（请求重放/构造）。
- 性能基准（`criterion`）、内存/吞吐与 Node 版对比报告，多平台发布二进制。
- **产出**：功能与性能对齐，进入可发布状态。

---

### 优先级原则
1. 先**端到端可用**（M0–M4）再**广覆盖**（M5–M8）。
2. 每实现一个协议，配一条**兼容性测试**。
3. 高风险项（H2 MITM、JS 正则兼容、插件生态）尽早做技术预研（spike），避免后期返工。

### 暂不承诺
- 1:1 复刻全部约 90 个协议的边角语义；
- Weinre 等历史工具的完整复刻；
- whistle 私有 JS 插件 API 的二进制兼容。
