//! whistle-rs 代理内核。
//!
//! 对应 whistle 的 `lib/index.js`、`init.js`、`handlers/`、`tunnel.js` 等。
//! 负责装配各子系统并驱动请求生命周期。设计见仓库 `docs/02-architecture.md`。
//!
//! 当前处于 M0 脚手架阶段：仅定义配置与生命周期入口的占位实现。

pub mod config;

pub use config::Config;

/// 内核错误类型。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 尚未实现的功能（脚手架阶段占位）。
    #[error("尚未实现: {0}")]
    NotImplemented(&'static str),
}

/// 内核操作结果类型。
pub type Result<T> = std::result::Result<T, Error>;

/// 启动代理服务（占位）。
///
/// M1 将在此监听 [`Config::bind_addr`] 并接入请求处理管线。
pub fn start(config: &Config) -> Result<()> {
    tracing::info!(addr = %config.bind_addr(), "whistle-rs 代理内核启动（占位）");
    Err(Error::NotImplemented("proxy core (计划于 M1 实现)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bind_addr() {
        let cfg = Config::default();
        assert_eq!(cfg.bind_addr(), "127.0.0.1:8899");
    }
}
