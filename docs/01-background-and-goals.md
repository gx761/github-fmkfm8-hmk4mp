# 01 · 背景与目标

## 1.1 whistle 是什么

whistle（命令行 `w2`）是 [avwo/whistle](https://github.com/avwo/whistle) 开发的、基于 Node.js 的跨平台 Web 调试代理。它的定位类似 Fiddler / Charles，但以**纯命令行 + Web 管理界面 + 文本规则**为特色。核心能力：

- **代理抓包**：HTTP、HTTPS、HTTP/2、WebSocket、TCP 全协议抓包与改写。
- **规则引擎（Rule DSL）**：用类似 hosts 文件的文本语法描述 `匹配模式 操作协议://参数`，支持域名 / 路径 / 通配符 / 正则匹配。这是 whistle 区别于其它抓包工具的核心。
- **HTTPS 解密**：内置根证书（CA），对 HTTPS 流量做中间人（MITM）解密。
- **Web UI**：默认 `http://127.0.0.1:8899/`，集抓包列表（Network）、规则编辑、插件、工具于一体。
- **插件体系**：第三方插件可注册自定义规则协议与 UI，插件以独立服务形式通过 HTTP 与内核通信。
- **内置工具**：Composer（请求重放/构造）、Weinre（远程 DOM 调试）、Console、日志等。

### whistle 现有代码结构（`lib/`）

| 目录/文件 | 职责 |
| --- | --- |
| `lib/index.js` / `init.js` / `config.js` | 入口、初始化、配置管理 |
| `lib/handlers/` | 处理 HTTP/HTTPS 请求与响应的核心处理器 |
| `lib/https/` | TLS 证书管理与 HTTPS/MITM 处理 |
| `lib/inspectors/` | 流量检查 / 规则应用阶段（请求/响应改写） |
| `lib/rules/` | 规则解析与匹配（Rule DSL 引擎） |
| `lib/plugins/` | 插件框架 |
| `lib/service/` | 服务层与 Web UI 后端接口 |
| `lib/util/` | 工具函数 |
| `lib/socket-mgr.js` | WebSocket / Socket 连接管理 |
| `lib/tunnel.js` | CONNECT 隧道处理 |
| `lib/upgrade.js` | 协议升级（WebSocket upgrade）处理 |

## 1.2 本项目目标

1. **代理内核对等**：用 Rust 实现 HTTP/HTTPS/HTTP2/WS/TCP 抓包与转发，行为与 whistle 对齐。
2. **规则语法兼容**：尽最大努力兼容 whistle 的规则文本语法，使现有用户的规则可以平滑迁移。优先覆盖高频协议（见 [04-rules-engine.md](04-rules-engine.md)）。
3. **性能与资源**：相比 Node 版显著降低内存占用、提升高并发抓包下的吞吐与延迟可预测性。
4. **单二进制分发**：产出跨平台单文件可执行程序（Linux/macOS/Windows），无运行时依赖。
5. **可扩展插件**：提供插件机制（详见架构文档），并尽量兼容 whistle 现有插件协议。

## 1.3 非目标（至少首个大版本内不做）

- **不**追求 1:1 复刻 whistle 的全部约 90 个规则协议；按优先级分批实现。
- **不**重写 whistle 的前端 Web UI（初期复用其前端或提供精简内置 UI，详见架构文档）。
- **不**保证 whistle 内部 JS API / 私有插件钩子的二进制级兼容。
- **不**首发就支持 Weinre 等历史遗留工具（低优先级）。

## 1.4 兼容性策略

- 规则语法层面：以 whistle 官方文档的协议语义为准，建立**兼容性测试集**（同一条规则在 whistle 与 whistle-rs 下行为对比）。
- 证书层面：复用「安装根证书」的交互模型，CA 证书自行生成，文档说明从 whistle 迁移的方法。
- 配置/数据目录：定义独立目录（如 `~/.whistle-rs/`），不直接覆盖 whistle 的数据，便于并存对比。

## 1.5 成功判定标准（首个里程碑）

- 能作为系统代理工作，抓取并展示 HTTP/HTTPS 流量。
- 支持 `host`、`file`、`redirect`、`reqHeaders`/`resHeaders`、`statusCode` 等高频规则。
- HTTPS MITM 可用（动态签发叶子证书）。
- 提供最小 Web UI 查看抓包列表与单条请求详情。
