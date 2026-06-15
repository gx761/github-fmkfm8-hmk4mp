//! CA 生成与动态证书签发、rustls 集成（HTTPS MITM）
//!
//! 对应 whistle 的 lib/https。详见 docs/03-module-mapping.md。
//!
//! 当前处于 M0 脚手架阶段：仅占位，尚未实现。设计见仓库 `docs/`。

/// crate 版本（来自 Cargo 包版本）。
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
