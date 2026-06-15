//! 运行配置。
//!
//! M0 阶段只定义结构与默认值；后续里程碑接入文件/CLI 加载与热更新。

use serde::{Deserialize, Serialize};

/// 默认代理端口（与 whistle 的 8899 对齐，便于对比）。
pub const DEFAULT_PROXY_PORT: u16 = 8899;

/// 全局运行配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 代理监听端口。
    pub port: u16,
    /// 监听地址。
    pub host: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: DEFAULT_PROXY_PORT,
            host: "127.0.0.1".to_string(),
        }
    }
}

impl Config {
    /// `host:port` 形式的监听地址。
    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
