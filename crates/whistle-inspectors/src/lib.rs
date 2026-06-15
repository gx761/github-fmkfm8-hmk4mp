//! 各规则协议的请求/响应改写实现
//!
//! 对应 whistle 的 lib/inspectors。详见 docs/04-rules-engine.md。
//!
//! 当前处于 M0 脚手架阶段：仅占位，尚未实现。设计见仓库 `docs/`。

/// crate 版本（来自 Cargo 包版本）。
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
