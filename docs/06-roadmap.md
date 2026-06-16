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

## M4 · Web UI（最小可用）— ✅ 已完成
- [x] `whistle-web`：axum REST（`/api/info`、`/api/traffic` 列表/详情/清空、`/api/rules` 读写）
  + WebSocket `/ws` 实时推送抓包事件。
- [x] 内嵌精简前端（`ui/index.html`）：抓包列表 + 单请求详情（请求/响应头、命中规则）+ 规则编辑器。
- [x] 规则热更新：编辑保存即生效（proxy 与 web 共享 `Arc<RwLock<RuleSet>>`），端到端验证通过。
- [x] CLI：`--ui-port`（默认 8900）/`--no-ui`。
- **产出**：浏览器打开 `http://127.0.0.1:8900/` 即可看流量、改规则。

## M5 · WebSocket / TCP / 上游代理 — 🟡 进行中
- [x] WebSocket：`ws://`（HTTP 代理路径）与 `wss://`（MITM 解密后）升级转发；
  转发握手、101 后双向中继升级连接，记录为 `ws`/`wss` 流量。端到端验证 ws 回显通过。
- [x] TCP：`--no-decrypt` 下 CONNECT 走盲隧道（字节级双向转发）。
- [x] 上游 HTTP 代理 `proxy://`（明文 HTTP，absolute-form 转发）；端到端验证两级代理链路通过。
- 待补：WS 帧级抓取（当前为字节级中继）；HTTPS 经上游代理、`socks`/`pac`、本地 SOCKS5 入站。
- **产出**：浏览器 WebSocket 可经代理正常工作；支持级联到上游 HTTP 代理。

## M6 · P1 协议横向铺开 — 🟡 进行中（主体已完成）
- [x] Body 改写与注入：`reqBody`/`resBody`、`reqReplace`/`resReplace`、
  `reqPrepend`/`reqAppend`/`resPrepend`/`resAppend`、`html*`/`js*`/`css*`（Prepend/Append/Body）。
  （仅当命中 body 改写规则时才缓冲 body，否则保持流式转发。）
- [x] 头/Cookie：`reqCookies`/`resCookies`、`attachment`（`referer`/`ua` 已于 M2 完成）。
- [x] 网络模拟：`reqDelay`/`resDelay`（`reqSpeed`/`resSpeed` 待补）。
- [x] 状态/方法：`replaceStatus`、`method`。
- 待补：`headerReplace`/`forwardedFor`/`*Cors`/`auth`/`*Charset`、`urlParams`/`pathReplace`/
  `locationHref`、规则注入类（`reqRules`/`resRules`/`includeFilter`/`excludeFilter`/`skip`）。
- 端到端验证：resReplace+html(Pre/Ap)pend、resBody、replaceStatus 均生效。
- **产出**：覆盖绝大多数日常调试场景（mock body、注入脚本、改状态、延迟）。

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
