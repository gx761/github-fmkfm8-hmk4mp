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
- [x] WebSocket **帧级抓取**：转发与解析解耦（原样转发，旁路解析帧），记录每条
  文本/控制消息的方向/opcode/预览/大小到抓包，UI 展示；端到端验证 send/recv 消息抓取通过。
- [x] TCP：`--no-decrypt` 下 CONNECT 走盲隧道（字节级双向转发）。
- [x] 上游 HTTP 代理 `proxy://`：明文 HTTP（absolute-form 转发）与 **HTTPS（先 CONNECT 打隧道再 TLS）**；
  端到端验证两级代理链路（含 MITM→上游代理→TLS 源站）通过。
- [x] **本地 SOCKS5 入站**（同端口区分 SOCKS5/HTTP）：no-auth + CONNECT，TLS 走 MITM、其余盲隧道；
  端到端验证 `curl --socks5-hostname` 的 https(MITM) 与 http(隧道) 均通过。
- [x] **上游 SOCKS5 代理 `socks://`**（no-auth，域名由上游解析）：HTTP 走隧道、HTTPS 隧道后再 TLS；
  端到端验证 A→socks://→B(SOCKS5 入站)→源站 通过。
- 已决定不实现：`pac`（需内置 JS 引擎评估 PAC 脚本，超出 clean-room Rust 重写范围）。
- **产出**：浏览器 WebSocket 可经代理正常工作；支持级联到上游 HTTP 代理。

## M6 · P1 协议横向铺开 — 🟡 进行中（主体已完成）
- [x] Body 改写与注入：`reqBody`/`resBody`、`reqReplace`/`resReplace`、
  `reqPrepend`/`reqAppend`/`resPrepend`/`resAppend`、`html*`/`js*`/`css*`（Prepend/Append/Body）。
  （仅当命中 body 改写规则时才缓冲 body，否则保持流式转发。）
- [x] 头/Cookie：`reqCookies`/`resCookies`、`attachment`（`referer`/`ua` 已于 M2 完成）。
- [x] 网络模拟：`reqDelay`/`resDelay`（`reqSpeed`/`resSpeed` 待补）。
- [x] 状态/方法/路径：`replaceStatus`、`method`、`urlParams`、`pathReplace`、`locationHref`。
- [x] 头/CORS/鉴权：`headerReplace`、`forwardedFor`、`reqCors`/`resCors`、`auth`、`delete`。
- [x] 控制/过滤类：`ignore://`（剔除协议，`*` 全剔）、`includeFilter`/`excludeFilter`
  （规则级门控，支持匹配模式与 `m:METHOD`）、`tpl`/`xtpl`（JSONP 模板）、`@include`（规则文件内联）。
- [x] 限速：`reqSpeed`/`resSpeed`（KB/s；正文缓冲时按总量近似时延）。
- 已决定不实现：`reqRules`/`resRules`/`inherit`（运行时动态注入/继承规则集，属高耦合的
  whistle 内部机制，价值有限）；`skip`/`enable`/`disable` 的完整 whistle 语义（与具体内置
  行为强绑定）——常见诉求已由 `ignore://` 覆盖。
- 端到端验证：resReplace+html(Pre/Ap)pend、resBody、replaceStatus、urlParams、auth、CORS 均生效。
- **产出**：覆盖绝大多数日常调试场景（mock body、注入脚本、改状态/路径、延迟、CORS、鉴权）。

## 工程/可用性增强（里程碑外）
- [x] 抽出独立的 `whistle-inspectors` crate（兑现 docs/03 的模块拆分计划）。
- [x] 抓包请求/响应正文与 WebSocket 帧级消息记录，Web UI 展示。
- [x] `w2r stop`/`status`（PID 文件）、`w2r start --config <file.toml>`（TOML 配置）。
- [x] Composer：Web UI 构造请求并经由本机代理回放（自动套用规则与抓包）。
- [x] `reqCharset`/`resCharset` 协议。
- [x] 端到端集成测试（真实 TCP 上的代理转发/mock/body 改写）。

## M7 · 插件体系 — 🟡 进行中（核心已完成）
- [x] `whistle-plugin`：whistle-rs 自有契约的「插件即本地 HTTP 服务」——
  命中 `plugin://<name>` 时把请求序列化为 JSON `POST` 到插件 `/handle`，
  据插件返回的 `{status,headers,body}` 产生响应（编程式 mock）。
- [x] 插件地址经配置 `[plugins]` 映射；示例见 `examples/plugin_example.py`；端到端验证通过。
- [x] `plugin-vars://`：把 `k=v` 变量透传给插件（PluginRequest.vars）。
- 待补：`pipe`（流式管道插件）；与 whistle npm 插件的兼容层（按需）。
- **产出**：可用外部 HTTP 插件对请求编程式应答。

## M8 · HTTP/2 与 P2 协议、打磨
- [x] HTTP/2 MITM（客户端侧）：MITM 证书 ALPN 提供 `h2`/`http-1.1`，按协商以 hyper http2
  服务客户端；上游仍走 http/1.1。端到端验证 `curl --http2` 经代理得到 HTTP/2 200。
- [x] `cache://`（Cache-Control）；其余 `*Type`/`*Charset`/`replaceStatus` 等已在 M6/增强中完成。
- [x] Composer（请求重放/构造）。
- [x] 性能基准（`criterion`，见 `crates/whistle-rules/benches/`）。
- [x] 多平台发布二进制（`release.yml`）+ Dockerfile。
- 已决定不实现（清单与理由）：
  - `reqWrite*`/`resWrite*`：whistle 用于把报文写入磁盘做日志，本项目以抓包存储 + Web UI 取代；
  - `responseFor`：依赖「引用另一条已抓请求的响应」，属高耦合特性，价值低；
  - `trailers`：HTTP trailer 改写，使用面极窄；
  - `cipher`/`sniCallback`：底层 TLS 细调，rustls 下非常用；
  - `weinre`：whistle 内置的历史远程调试工具，已过时；
  - `pac`：需内置 JS 引擎评估 PAC 脚本。
- **产出**：功能覆盖日常调试主线，性能可度量，进入可发布状态。

---

### 优先级原则
1. 先**端到端可用**（M0–M4）再**广覆盖**（M5–M8）。
2. 每实现一个协议，配一条**兼容性测试**。
3. 高风险项（H2 MITM、JS 正则兼容、插件生态）尽早做技术预研（spike），避免后期返工。

### 暂不承诺
- 1:1 复刻全部约 90 个协议的边角语义；
- Weinre 等历史工具的完整复刻；
- whistle 私有 JS 插件 API 的二进制兼容。
