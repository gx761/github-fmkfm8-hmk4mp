//! whistle-rs 代理内核。
//!
//! 对应 whistle 的 `lib/index.js`、`init.js`、`handlers/`、`tunnel.js` 等。
//! 负责装配各子系统并驱动请求生命周期。设计见仓库 `docs/02-architecture.md`。
//!
//! M1：可作为系统代理转发明文 HTTP 请求并记录抓包；HTTPS（CONNECT）先做盲隧道
//! 转发（解密在 M3 实现）。

pub mod config;
pub mod proxy;

use std::sync::Arc;

pub use config::Config;
pub use whistle_capture::CaptureStore;

/// 内核错误类型。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// IO / 绑定错误。
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 内核操作结果类型。
pub type Result<T> = std::result::Result<T, Error>;

/// 启动代理服务并阻塞运行，直到收到 Ctrl-C。
pub async fn start(config: Config) -> Result<()> {
    let store = Arc::new(CaptureStore::new(config.capture_capacity));
    proxy::serve(config, store).await
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
