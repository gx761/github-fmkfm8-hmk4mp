//! 运行配置。
//!
//! M0 阶段只定义结构与默认值；后续里程碑接入文件/CLI 加载与热更新。

use serde::{Deserialize, Serialize};

/// 默认代理端口（与 whistle 的 8899 对齐，便于对比）。
pub const DEFAULT_PROXY_PORT: u16 = 8899;

/// 抓包存储默认容量（最近 N 条）。
pub const DEFAULT_CAPTURE_CAPACITY: usize = 5000;

/// 抓包正文预览默认上限（字节）。超过此长度的已知长度正文不缓冲（保持流式）。
pub const DEFAULT_CAPTURE_BODY_LIMIT: usize = 512 * 1024;

/// Web 管理界面默认端口。
pub const DEFAULT_UI_PORT: u16 = 8900;

/// 全局运行配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 代理监听端口。
    pub port: u16,
    /// 监听地址。
    pub host: String,
    /// 抓包存储容量（最近 N 条）。
    pub capture_capacity: usize,
    /// 抓包正文预览上限（字节）。
    pub capture_body_limit: usize,
    /// 规则文件路径（可选）。
    pub rules_file: Option<String>,
    /// 数据目录（CA、配置等）；None 时用 `~/.whistle-rs`。
    pub data_dir: Option<String>,
    /// 是否对 HTTPS（CONNECT）做中间人解密；false 时退化为盲隧道。
    pub decrypt_https: bool,
    /// Web 管理界面端口。
    pub ui_port: u16,
    /// 是否启用 Web 管理界面。
    pub ui_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: DEFAULT_PROXY_PORT,
            host: "127.0.0.1".to_string(),
            capture_capacity: DEFAULT_CAPTURE_CAPACITY,
            capture_body_limit: DEFAULT_CAPTURE_BODY_LIMIT,
            rules_file: None,
            data_dir: None,
            decrypt_https: true,
            ui_port: DEFAULT_UI_PORT,
            ui_enabled: true,
        }
    }
}

impl Config {
    /// Web 管理界面监听地址。
    pub fn ui_addr(&self) -> String {
        format!("{}:{}", self.host, self.ui_port)
    }
}

impl Config {
    /// `host:port` 形式的监听地址。
    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
