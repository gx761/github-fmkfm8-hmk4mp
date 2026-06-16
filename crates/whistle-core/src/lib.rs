//! whistle-rs 代理内核。
//!
//! 对应 whistle 的 `lib/index.js`、`init.js`、`handlers/`、`tunnel.js` 等。
//! 负责装配各子系统并驱动请求生命周期。设计见仓库 `docs/02-architecture.md`。
//!
//! M1：HTTP 转发代理 + 抓包；M2：规则引擎 + P0 协议；
//! M3：HTTPS 中间人解密（动态签发证书）。

pub mod config;
pub mod proxy;
mod ws;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

pub use config::Config;
pub use whistle_capture::CaptureStore;
pub use whistle_rules::RuleSet;
pub use whistle_tls::CertAuthority;
pub use whistle_web::WebState;

/// 内核错误类型。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// IO / 绑定错误。
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    /// 规则解析错误。
    #[error("规则解析失败: {0}")]
    Rules(String),
    /// TLS / 证书错误。
    #[error("TLS 错误: {0}")]
    Tls(String),
}

/// 内核操作结果类型。
pub type Result<T> = std::result::Result<T, Error>;

/// 解析数据目录。
pub fn data_dir(config: &Config) -> PathBuf {
    config
        .data_dir
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(whistle_tls::default_data_dir)
}

/// 从配置加载规则集与其文本（无规则文件则返回空集与空串）。
pub fn load_rules(config: &Config) -> Result<(RuleSet, String)> {
    match &config.rules_file {
        Some(path) => {
            let text = std::fs::read_to_string(path)?;
            let rules = RuleSet::parse(&text).map_err(|e| Error::Rules(e.to_string()))?;
            tracing::info!(path = %path, count = rules.len(), "已加载规则");
            Ok((rules, text))
        }
        None => Ok((RuleSet::default(), String::new())),
    }
}

/// 加载或生成根 CA。
pub fn load_ca(config: &Config) -> Result<Arc<CertAuthority>> {
    let dir = data_dir(config);
    let ca = CertAuthority::load_or_generate(&dir).map_err(|e| Error::Tls(e.to_string()))?;
    Ok(Arc::new(ca))
}

/// 启动代理服务（并按需启动 Web 管理界面），阻塞运行直到收到 Ctrl-C。
pub async fn start(config: Config) -> Result<()> {
    let store = Arc::new(CaptureStore::new(config.capture_capacity));
    let (rule_set, rules_text) = load_rules(&config)?;
    let rules = Arc::new(RwLock::new(rule_set));
    let ca = load_ca(&config)?;

    // Web 管理界面（共享 store 与 rules，支持热更新）。
    if config.ui_enabled {
        let state = WebState {
            store: store.clone(),
            rules: rules.clone(),
            rules_text: Arc::new(RwLock::new(rules_text)),
        };
        let ui_addr = config.ui_addr();
        match ui_addr.parse::<std::net::SocketAddr>() {
            Ok(addr) => {
                tracing::info!(%ui_addr, "管理界面 → http://{ui_addr}/");
                tokio::spawn(async move {
                    if let Err(e) = whistle_web::serve(addr, state).await {
                        tracing::error!(%e, "管理界面退出");
                    }
                });
            }
            Err(e) => tracing::warn!(%ui_addr, %e, "管理界面地址非法，已跳过"),
        }
    }

    proxy::serve(config, store, rules, ca).await
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
