# 03 · 模块映射（whistle → whistle-rs）

下表把 whistle（Node.js）的内部模块映射到本项目计划中的 Rust crate / 模块，并标注实现要点。

| whistle 模块 | 职责 | whistle-rs 对应 | 实现要点 |
| --- | --- | --- | --- |
| `lib/index.js`, `init.js` | 启动与装配 | `whistle-cli` + `whistle-core::App` | clap 解析子命令；构建 tokio 运行时与各子系统 |
| `lib/config.js` | 配置 | `whistle-core::config` | 配置结构体 + serde；数据目录 `~/.whistle-rs/` |
| `lib/handlers/` | 请求/响应处理器 | `whistle-core::handlers` | 以「中间件/管线」组织请求生命周期 |
| `lib/tunnel.js`, `upgrade.js` | CONNECT 隧道 / 协议升级 | `whistle-core::tunnel` + `whistle-proto` | CONNECT、WS upgrade、ALPN(h2) 判定 |
| `lib/socket-mgr.js` | Socket/WS 连接管理 | `whistle-core::conn` | tokio 连接池与生命周期管理 |
| `lib/https/` | TLS 证书与 MITM | `whistle-tls` | rcgen 生成 CA 与叶子证书；rustls 双向 TLS；证书缓存 |
| `lib/rules/` | 规则解析与匹配 | `whistle-rules` | DSL 词法/语法解析；模式匹配（域名/路径/通配/正则） |
| `lib/inspectors/` | 规则应用（改写） | `whistle-inspectors` | 每个规则协议一个 inspector，按请求/响应阶段执行 |
| `lib/plugins/` | 插件框架 | `whistle-plugin` | 兼容「插件即本地 HTTP 服务」；后续评估 WASM |
| `lib/service/` | 服务层 / UI 后端 | `whistle-web` | axum REST + WebSocket 实时推送 |
| `lib/util/` | 工具函数 | 各 crate 内 `util` / 公共 `whistle-util` | 按需拆分，避免巨型 util |
| 抓包数据/Network 面板数据 | 流量记录 | `whistle-capture` | 流量数据模型、环形缓冲、落盘、查询 |
| 上游代理 / PAC / SOCKS | 出站路由 | `whistle-core::upstream` | direct / http-proxy / socks5 / PAC 求值 |

## 关键差异与取舍

- **事件循环 → tokio**：whistle 的单线程回调模型改为 tokio 多线程异步；共享状态用 `Arc<...>` + 无锁/细粒度锁（`dashmap`、`RwLock`）。
- **Buffer 改写 → 流式零拷贝**：whistle 常把 body 读入内存再改写；whistle-rs 默认流式转发，仅在规则需要时（body 注入/替换）才缓冲，且受大小阈值约束。
- **证书**：whistle 依赖 node-forge/OpenSSL；whistle-rs 用 `rcgen` 程序化签发，纯 Rust，无外部 OpenSSL 依赖。
- **插件模型**：whistle 插件本身是 npm 包并起本地服务；为兼容，whistle-rs 首版以「按约定的 HTTP 接口调用外部插件进程」对接，不要求插件用 Rust 重写。
- **前端**：UI 后端用 axum 重写，前端首版可复用 whistle 的静态资源或提供精简版，避免一次性重写前端工程。
