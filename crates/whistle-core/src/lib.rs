//! whistle-rs 代理内核。
//!
//! 对应 whistle 的 `lib/index.js`、`init.js`、`handlers/`、`tunnel.js` 等。
//! 负责装配各子系统并驱动请求生命周期。设计见仓库 `docs/02-architecture.md`。
//!
//! M1：HTTP 转发代理 + 抓包；CONNECT 盲隧道。
//! M2：规则引擎（DSL 匹配）+ P0 协议（host/redirect/file/statusCode/
//! reqHeaders/resHeaders/reqType/resType）。

pub mod apply;
pub mod config;
pub mod proxy;

use std::sync::Arc;

pub use config::Config;
pub use whistle_capture::CaptureStore;
pub use whistle_rules::RuleSet;

/// 内核错误类型。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// IO / 绑定错误。
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    /// 规则解析错误。
    #[error("规则解析失败: {0}")]
    Rules(String),
}

/// 内核操作结果类型。
pub type Result<T> = std::result::Result<T, Error>;

/// 从配置加载规则集（无规则文件则返回空集）。
pub fn load_rules(config: &Config) -> Result<Arc<RuleSet>> {
    match &config.rules_file {
        Some(path) => {
            let text = std::fs::read_to_string(path)?;
            let rules = RuleSet::parse(&text).map_err(|e| Error::Rules(e.to_string()))?;
            tracing::info!(path = %path, count = rules.len(), "已加载规则");
            Ok(Arc::new(rules))
        }
        None => Ok(Arc::new(RuleSet::default())),
    }
}

/// 启动代理服务并阻塞运行，直到收到 Ctrl-C。
pub async fn start(config: Config) -> Result<()> {
    let store = Arc::new(CaptureStore::new(config.capture_capacity));
    let rules = load_rules(&config)?;
    proxy::serve(config, store, rules).await
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
