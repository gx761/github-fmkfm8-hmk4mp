//! 插件协议与调度（兼容『插件即本地 HTTP 服务』）
//!
//! 对应 whistle 的 lib/plugins。
//!
//! 当前处于 M0 脚手架阶段：仅占位，尚未实现。设计见仓库 `docs/`。

/// crate 版本（来自 Cargo 包版本）。
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
