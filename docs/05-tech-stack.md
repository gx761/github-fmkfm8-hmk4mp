# 05 · 技术选型（Rust crate）

> 以下为计划选型，最终以实现时锁定的版本为准。优先选择社区成熟、维护活跃、纯 Rust 的库，减少 C 依赖以保证跨平台单二进制分发。

## 5.1 核心运行时与网络

| 用途 | 选型 | 备注 |
| --- | --- | --- |
| 异步运行时 | `tokio`（multi-thread） | 网络 IO、定时、任务调度 |
| HTTP 类型/底层 | `hyper` 1.x + `http` | 服务端/客户端低层 HTTP/1.1、HTTP/2 |
| HTTP 客户端（上游） | `hyper-util` + 自建连接器 / 备选 `reqwest` | 需要精细控制连接复用、代理、MITM |
| WebSocket | `tokio-tungstenite` | WS 升级与帧级抓取 |
| 字节处理 | `bytes` | 零拷贝缓冲 |

## 5.2 TLS / 证书（MITM 关键）

| 用途 | 选型 | 备注 |
| --- | --- | --- |
| TLS 实现 | `rustls` + `tokio-rustls` | 纯 Rust，无 OpenSSL；支持 ALPN（h2 协商） |
| 证书生成 | `rcgen` | 程序化生成根 CA 与按域名动态签发叶子证书 |
| 证书解析/缓存 | `rustls-pemfile` + 自建 LRU 缓存 | 叶子证书按 SNI 缓存复用 |
| SOCKS5 | 自实现 / `tokio-socks`（客户端侧） | 上游 SOCKS 与本地 SOCKS 入站 |

## 5.3 规则引擎

| 用途 | 选型 | 备注 |
| --- | --- | --- |
| 解析 | 手写解析 或 `nom` / `pest` | DSL 行解析、模式分类 |
| 正则 | `regex` | 正则模式匹配（注意与 JS 正则差异，需做兼容层） |
| 通配/glob | `globset` 或自实现 | 域名/路径通配 |
| 并发索引 | `dashmap` | 规则命中缓存、连接表 |

## 5.4 管理面 / Web

| 用途 | 选型 | 备注 |
| --- | --- | --- |
| Web 框架 | `axum` | REST API + WebSocket 推送，基于 hyper/tokio |
| 静态资源 | `rust-embed` | 内嵌前端到二进制 |
| 实时推送 | axum WebSocket | 抓包事件流推送到 UI |

## 5.5 序列化 / 配置 / CLI / 可观测

| 用途 | 选型 |
| --- | --- |
| 序列化 | `serde` + `serde_json` |
| 配置文件 | `toml` / `serde` |
| CLI | `clap`（derive） |
| 日志 | `tracing` + `tracing-subscriber` |
| 错误处理 | `thiserror`（库）/ `anyhow`（应用层） |
| 时间 | `time` 或 `chrono` |

## 5.6 测试 / 工程

| 用途 | 选型 |
| --- | --- |
| 单测/集成 | 内置 `#[test]` + `tokio::test` |
| HTTP 行为对比 | 自建兼容性测试集（whistle vs whistle-rs） |
| 基准 | `criterion` |
| Lint/格式 | `clippy` + `rustfmt` |
| CI | GitHub Actions（fmt/clippy/test，多平台构建） |

## 5.7 待评估 / 风险点

- **JS 正则兼容**：whistle 规则用 JS 正则，`regex` crate 语法/特性有差异（如反向引用不支持），需建立兼容层或文档化差异。
- **HTTP/2 MITM**：H2 的多路复用 + 头压缩在 MITM 下实现复杂，列为 P1 风险项。
- **插件生态**：whistle 插件是 npm 包，Rust 侧无法直接加载；首版走「外部进程 + HTTP 约定」兼容，长期评估 WASM（`wasmtime`）。
- **前端复用**：直接复用 whistle 前端需对齐其 API 契约；若改动大，则做精简内置 UI。
