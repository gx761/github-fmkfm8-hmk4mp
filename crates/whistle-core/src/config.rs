//! 运行配置。
//!
//! M0 阶段只定义结构与默认值；后续里程碑接入文件/CLI 加载与热更新。

use std::collections::HashMap;

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
///
/// `#[serde(default)]` 使部分字段的 TOML 文件也能解析：缺失字段回落到 `Default`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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
    /// 是否启用 HTTPS 中间人解密能力；false 时所有 HTTPS 一律盲隧道直通。
    ///
    /// 注意：启用本能力后，**默认仅对命中规则的 host 做解密**（其余盲隧道直通），
    /// 以免未安装根证书时所有 HTTPS 站点打不开。要解密全部 HTTPS，另见
    /// [`Config::intercept_all_https`]。
    pub decrypt_https: bool,
    /// 是否对**所有** HTTPS host 做中间人解密（whistle 的「Intercept HTTPS CONNECTs」）。
    ///
    /// 默认 false：只解密命中规则的 host。开启需先安装并信任根证书，否则 HTTPS 站点
    /// 会因证书不受信任而无法访问。
    pub intercept_all_https: bool,
    /// 是否对客户端启用 HTTP/2（MITM 证书 ALPN 提供 h2）；默认否（兼容 WebSocket）。
    pub enable_http2: bool,
    /// Web 管理界面端口。
    pub ui_port: u16,
    /// 是否启用 Web 管理界面。
    pub ui_enabled: bool,
    /// UI 模式：`native`（内置精简界面）或 `whistle`（内嵌 whistle 原生前端）。
    pub ui_mode: String,
    /// 插件映射：`plugin://<name>` → 插件 HTTP 服务地址 `host:port`。
    pub plugins: HashMap<String, String>,
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
            intercept_all_https: false,
            enable_http2: false,
            ui_port: DEFAULT_UI_PORT,
            ui_enabled: true,
            ui_mode: "native".to_string(),
            plugins: HashMap::new(),
        }
    }
}

impl Config {
    /// 从 TOML 文件加载配置。
    ///
    /// 缺失字段回落到 `Default`（依赖结构体上的 `#[serde(default)]`）。
    /// IO 与解析错误统一映射为 `String`。
    pub fn from_toml_file(path: &str) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("读取配置失败: {e}"))?;
        toml::from_str(&text).map_err(|e| format!("解析配置失败: {e}"))
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_toml_falls_back_to_default() {
        // 仅提供部分字段，其余应回落到 Default。
        let cfg: Config = toml::from_str("port = 1234\nui_port = 4321").unwrap();
        assert_eq!(cfg.port, 1234);
        assert_eq!(cfg.ui_port, 4321);
        // 未提供的字段使用默认值。
        assert!(cfg.decrypt_https);
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.capture_capacity, DEFAULT_CAPTURE_CAPACITY);
        assert!(cfg.rules_file.is_none());
    }

    #[test]
    fn from_toml_file_reads_and_parses() {
        // 写入临时文件并通过 from_toml_file 加载。
        let mut path = std::env::temp_dir();
        path.push(format!("whistle-rs-test-{}.toml", std::process::id()));
        std::fs::write(&path, "port = 1234\nui_port = 4321").unwrap();

        let cfg = Config::from_toml_file(path.to_str().unwrap()).unwrap();
        assert_eq!(cfg.port, 1234);
        assert_eq!(cfg.ui_port, 4321);
        assert!(cfg.decrypt_https);
        assert_eq!(cfg.host, "127.0.0.1");

        let _ = std::fs::remove_file(&path);
    }
}
