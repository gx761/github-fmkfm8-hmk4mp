# whistle-rs

> 用 Rust 重构 [whistle](https://github.com/avwo/whistle)（`w2`）—— 一个跨平台的 HTTP / HTTPS / HTTP2 / WebSocket / TCP 抓包调试代理工具。

**当前阶段：🚧 M0 工程脚手架已完成 —— Cargo workspace + `w2r` CLI 骨架 + CI 就绪，代理内核功能从 M1 起逐步实现。**

## 快速开始

```bash
# 构建
cargo build --workspace

# 查看 CLI（功能逐步在后续里程碑落地）
cargo run --bin w2r -- --help

# 检查（与 CI 一致）
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 工程结构

```
Cargo.toml                 # workspace
crates/
├── whistle-cli/           # `w2r` 命令行入口（二进制）
├── whistle-core/          # 代理内核：监听/连接/转发/MITM 调度
├── whistle-rules/         # 规则 DSL：解析/匹配/求值
├── whistle-inspectors/    # 各规则协议的请求/响应改写
├── whistle-tls/           # CA 生成与动态证书签发（HTTPS MITM）
├── whistle-proto/         # HTTP/1.1·H2·WS·SOCKS 协议处理
├── whistle-capture/       # 抓包数据模型/存储/事件流
├── whistle-plugin/        # 插件协议与调度
└── whistle-web/           # axum 管理面 API + UI + WS 推送
docs/                      # 设计文档（见下）
```

whistle 是基于 Node.js 的网络调试代理，核心能力包括：基于规则（Rule DSL）的请求/响应改写、HTTPS 中间人解密、Web UI 抓包面板、插件体系、Composer（请求重放）、Weinre 等。本项目的目标是用 Rust 重写其代理内核，获得更低的内存占用、更高的吞吐与更强的稳定性，同时尽量保持 whistle 规则语法的兼容性。

## 为什么用 Rust 重写

| 维度 | Node.js 版 whistle | Rust 重写目标 |
| --- | --- | --- |
| 内存占用 | 单进程常驻数十~上百 MB | 显著降低（无 V8/GC 常驻） |
| 并发模型 | 单线程事件循环 | tokio 多线程异步 + work-stealing |
| 大流量/高并发抓包 | GC 抖动、易 OOM | 零拷贝转发、可预测延迟 |
| 分发 | 依赖 Node 运行时 | 单二进制，跨平台静态分发 |
| TLS 性能 | OpenSSL via Node | rustls（纯 Rust，内存安全） |

## 设计文档

| 文档 | 内容 |
| --- | --- |
| [docs/01-background-and-goals.md](docs/01-background-and-goals.md) | 背景、目标、非目标、兼容性策略 |
| [docs/02-architecture.md](docs/02-architecture.md) | 目标整体架构与数据流 |
| [docs/03-module-mapping.md](docs/03-module-mapping.md) | whistle 各模块 → Rust crate/模块 对应方案 |
| [docs/04-rules-engine.md](docs/04-rules-engine.md) | 规则引擎设计与协议覆盖清单 |
| [docs/05-tech-stack.md](docs/05-tech-stack.md) | Rust 技术/crate 选型 |
| [docs/06-roadmap.md](docs/06-roadmap.md) | 分阶段里程碑路线图 |

## 工作名 / 命名

- 项目代号：`whistle-rs`
- 计划二进制名：`w2r`（与现有 `w2` 区分，便于并存对比）

## 许可证

计划沿用 whistle 的 MIT 许可证（待最终确认）。
