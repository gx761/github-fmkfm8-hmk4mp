# whistle-rs

> 用 Rust 重构 [whistle](https://github.com/avwo/whistle)（`w2`）—— 一个跨平台的 HTTP / HTTPS / HTTP2 / WebSocket / TCP 抓包调试代理工具。

**当前阶段：✅ 可用的 v0.1 —— HTTP/HTTPS 抓包代理（含中间人解密）、约 30 个规则协议、WebSocket、级联上游代理、Web 管理界面均已实现并通过端到端测试。**（里程碑进度见 [docs/06-roadmap.md](docs/06-roadmap.md)）

## 快速开始

```bash
# 构建
cargo build --workspace

# 启动代理（默认端口 8899，开启 HTTPS 解密）+ 管理界面（默认 8900）
cargo run --bin w2r -- start --port 8899 --rules examples/rules.txt
#   打开 http://127.0.0.1:8900/ 查看实时抓包、编辑规则（保存即生效）

# 可选：对客户端启用 HTTP/2（默认关闭，以兼容 WebSocket 等）
cargo run --bin w2r -- start --http2

# 可选：内嵌 whistle 原生前端（Network 面板可用：无报错、实时显示抓包；规则/值编辑暂只读）
cargo run --bin w2r -- start --ui whistle
#   服务 whistle 2.10.4 编译好的前端 + 兼容的 /cgi-bin 后端适配层

# 把浏览器/系统 HTTP 代理指向 127.0.0.1:8899 即可抓 HTTP 流量。
# 抓 HTTPS：先导出并信任根证书
cargo run --bin w2r -- ca export whistle-rs-ca.pem
#   将 whistle-rs-ca.pem 导入系统/浏览器的“受信任根证书”，再访问 https 站点。

# 检查（与 CI 一致）
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# 容器运行（代理 8899 / 管理界面 8900）
docker build -t whistle-rs . && docker run --rm -p 8899:8899 -p 8900:8900 whistle-rs
```

> 发布：推送 `v*` tag 触发 `.github/workflows/release.yml`，自动构建 Linux/macOS(x86_64+arm64)/Windows 的 `w2r` 二进制并附加到 GitHub Release。

## 已实现能力

- **代理**：HTTP / HTTPS 抓包与转发；HTTPS 中间人解密（动态签发证书，`w2r ca export` 安装根证书后即可）；**HTTP/2**（客户端侧按 ALPN 协商 h2）；CONNECT 盲隧道（`--no-decrypt`）。
- **WebSocket**：`ws://` 与 `wss://` 升级转发，并做**帧级抓取**（记录每条消息的方向/类型/预览/大小）。
- **级联代理**：`proxy://`（上游 HTTP 代理）与 `socks://`（上游 SOCKS5 代理），HTTP 与 HTTPS 均支持。
- **SOCKS5 入站**：与 HTTP 代理共用端口；`curl --socks5-hostname` 的 HTTPS 走 MITM、其余盲隧道。
- **插件**：`plugin://<name>` 把请求 POST 给外部插件 HTTP 服务，按其 JSON 返回编程式应答（示例 `examples/plugin_example.py`）。
- **正文抓取**：记录请求/响应正文预览（已知长度且不超限时缓冲，默认 512KB，否则保持流式）。
- **Web 管理界面**：①内置精简界面（默认）：实时抓包列表 + 请求详情（头 / 正文 / WebSocket 消息 / 命中规则）+ 在线规则编辑（保存即热生效）+ **Composer**；②`--ui whistle`（实验性）：内嵌 whistle 原生前端 + whistle 兼容 `/cgi-bin` 后端适配层，复用 whistle 真实 UI。
- **CLI**：`w2r start` / `stop` / `status`（基于 PID 文件）/ `ca export` / `ca path`；`start --config <file.toml>` 从 TOML 加载配置。
- **规则引擎**：正则 / 通配符 / URL 前缀 / 域名+路径 匹配；约 44 个协议（含控制/过滤）：

  | 类别 | 协议 |
  | --- | --- |
  | 转发 / Mock | `host` `redirect` `file` `rawfile` `statusCode` `proxy` |
  | 头 / Cookie | `reqHeaders` `resHeaders` `reqCookies` `resCookies` `reqType` `resType` `reqCharset` `resCharset` `ua` `referer` `attachment` `auth` `reqCors` `resCors` `forwardedFor` `headerReplace` `delete` |
  | Body 改写 | `reqBody` `resBody` `reqReplace` `resReplace` `*Prepend` `*Append` `html*`/`js*`/`css*` |
  | 状态 / 方法 / 路径 | `replaceStatus` `method` `urlParams` `pathReplace` `locationHref` |
  | 时延 / 限速 / 缓存 | `reqDelay` `resDelay` `reqSpeed` `resSpeed` `cache` |
  | 上游 / 插件 | `proxy` `socks` `plugin` `plugin-vars` |
  | 模板 / 控制 | `tpl` `xtpl` `ignore` `includeFilter` `excludeFilter` + `@include` 行 |

- **测试**：54 个单元/集成测试 + 规则引擎 criterion 基准（`crates/whistle-rules/benches/`）。

> 已决定不实现（详见路线图，附理由）：`pac`（需 JS 引擎）、`reqWrite*`/`resWrite*`、`responseFor`、`trailers`、`cipher`/`sniCallback`、`weinre`、`pipe`、上游侧 HTTP/2；`reqRules`/`resRules`/`inherit`（动态规则注入）尚未实现。

## 工程结构

```
Cargo.toml                 # workspace
crates/
├── whistle-cli/           # `w2r` 命令行入口（二进制）✅
├── whistle-core/          # 代理内核：监听/转发/MITM/WS 中继/抓包调度 ✅
├── whistle-rules/         # 规则 DSL：解析/匹配/求值 ✅
├── whistle-inspectors/    # 各规则协议的请求/响应改写实现 ✅
├── whistle-tls/           # CA 生成与动态证书签发（HTTPS MITM）✅
├── whistle-capture/       # 抓包数据模型/存储/事件流 ✅
├── whistle-web/           # axum 管理面 API + UI + WS 推送 ✅
├── whistle-proto/         # （占位）HTTP/2·SOCKS 等协议处理
└── whistle-plugin/        # （占位）插件协议与调度
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
