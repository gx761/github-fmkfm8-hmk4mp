# 04 · 规则引擎设计与协议覆盖

规则引擎是 whistle 的灵魂，也是本项目兼容性的核心。本文定义 DSL 解析/匹配模型，并按优先级列出协议覆盖计划。

## 4.1 规则语法（DSL）

whistle 规则文件每行一条规则，基本形式：

```
<匹配模式> <操作协议>://<参数> [操作协议2://参数2 ...]
```

- **匹配模式（pattern）**：可为
  - 域名 / 带路径的 URL：`www.example.com/path`
  - 通配符：`*.example.com`、`example.com/**`
  - 正则：`/^https?:\/\/.+\.cdn\.com/`
  - 精确路径 / 协议前缀 等。
- **操作（operator）**：`协议://参数`，可一行多个；也支持 `操作 模式` 的反向书写（whistle 两种顺序都接受，需兼容）。
- **行内特性**：注释（`#`）、`@` 引入外部规则文件、`includeFilter`/`excludeFilter` 过滤、`lineProps`（行级属性）、规则分组与启用/禁用。

### 解析与匹配管线

1. **词法/语法解析**：逐行解析为 `Rule { pattern, operations: Vec<Operation> }`。解析器用手写或 `nom`/`pest`，对模式类型（domain/wildcard/regex/path）归一化。
2. **索引**：按主机名建立快速索引，正则规则单列，降低每请求匹配成本。
3. **求值**：对一个请求收集所有命中规则，按 whistle 的优先级语义（精确度、书写顺序、`reqRules`/`resRules` 注入等）合并出最终生效的协议集合。
4. **冲突与覆盖**：同类协议多次命中时遵循 whistle 的「后者/更精确者覆盖」语义；建立兼容性测试固化行为。

## 4.2 协议覆盖清单（按优先级）

whistle 文档共约 90+ 个规则协议。下表按实现优先级分组（P0=首版必须，P1=次批，P2=后续/低频）。

### P0 — 首版必须（最高频）

| 分组 | 协议 |
| --- | --- |
| 转发/重定向 | `host`、`xhost`、`redirect`、`proxy`/`http`、`https`、`ws`、`wss` |
| 本地 Mock | `file`、`xfile`、`rawfile`、`tpl`、`statusCode` |
| 头部改写 | `reqHeaders`、`resHeaders`、`reqCookies`、`resCookies` |
| 类型 | `reqType`、`resType` |
| 控制 | `ignore`、`disable`、`enable`、`pattern`、`filters` |

### P1 — 次批（常用）

| 分组 | 协议 |
| --- | --- |
| 上游代理 | `https-proxy`、`socks`、`pac`、`xproxy`、`xhost`、`xsocks`、`xhttps-proxy` |
| Body 改写 | `reqBody`、`resBody`、`reqReplace`、`resReplace`、`reqAppend`/`reqPrepend`、`resAppend`/`resPrepend` |
| 注入 | `htmlAppend`/`htmlPrepend`/`htmlBody`、`jsAppend`/`jsPrepend`/`jsBody`、`cssAppend`/`cssPrepend`/`cssBody`、`style`、`reqScript`、`resScript`、`frameScript` |
| 头/Cookie 细节 | `headerReplace`、`referer`、`ua`、`forwardedFor`、`reqCors`、`resCors`、`auth`、`reqCharset`、`resCharset` |
| 网络模拟 | `reqDelay`、`resDelay`、`reqSpeed`、`resSpeed` |
| 状态/重写 | `replaceStatus`、`method`、`urlParams`、`pathReplace`、`locationHref`、`attachment` |
| 规则注入 | `reqRules`、`resRules`、`rule`、`inherit`、`includeFilter`、`excludeFilter`、`skip` |

### P2 — 后续/低频/高级

| 分组 | 协议 |
| --- | --- |
| 插件/扩展 | `plugin`、`plugin-vars`、`pipe`、`operation` |
| 原始写入 | `reqWrite`、`resWrite`、`reqWriteRaw`、`resWriteRaw`、`reqMerge`、`resMerge`、`responseFor` |
| TLS 高级 | `cipher`、`sniCallback`、`tunnel`、`trailers` |
| 调试工具 | `weinre`、`log` |
| 缓存/其它 | `cache`、`delete`、`protocols`、`xrawfile`、`xtpl`、`@`（引入文件）、`lineProps`、`reqCharset` 等 |

> 完整协议清单（来自 whistle `docs/docs/rules`，约 90+ 项）将在实现时逐条建表登记「语义 / 阶段（req/res）/ 兼容状态 / 测试用例」。

## 4.3 inspector 抽象

每个协议对应一个实现统一 trait 的 inspector：

```rust
// 伪代码，最终以 whistle-inspectors crate 为准
#[async_trait]
trait Inspector {
    /// 该 inspector 关心的阶段：请求侧 / 响应侧 / 连接前
    fn phase(&self) -> Phase;
    /// 对上下文应用本协议的改写；可短路（如返回本地响应）
    async fn apply(&self, ctx: &mut TrafficCtx, args: &OpArgs) -> InspectResult;
}
```

引擎按 `Phase` 顺序遍历命中的协议，调用对应 inspector。这样新增协议 = 新增一个 inspector + 登记到注册表，便于分批落地与测试。
