//! `w2r` —— whistle-rs 命令行入口。
//!
//! M0 提供命令骨架（start/stop/status/ca）。各子命令的实际能力在后续里程碑落地，
//! 详见仓库 `docs/06-roadmap.md`。

use clap::{Parser, Subcommand};
use whistle_core::Config;

/// whistle-rs：用 Rust 重写的 HTTP/HTTPS/WS 调试代理。
#[derive(Debug, Parser)]
#[command(name = "w2r", version, about)]
struct Cli {
    /// 提高日志详细程度（可叠加：-v, -vv）。
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 启动代理服务。
    Start {
        /// TOML 配置文件路径；提供时优先使用配置文件，忽略其它 start 参数。
        #[arg(long)]
        config: Option<String>,
        /// 监听地址（默认 127.0.0.1；容器中可设 0.0.0.0）。
        #[arg(long)]
        host: Option<String>,
        /// 代理监听端口。
        #[arg(short, long, default_value_t = whistle_core::config::DEFAULT_PROXY_PORT)]
        port: u16,
        /// 规则文件路径（whistle 规则语法）。
        #[arg(short, long)]
        rules: Option<String>,
        /// 数据目录（CA、配置）；默认 ~/.whistle-rs。
        #[arg(long)]
        data_dir: Option<String>,
        /// 不解密 HTTPS（CONNECT 退化为盲隧道）。
        #[arg(long)]
        no_decrypt: bool,
        /// 对客户端启用 HTTP/2（默认关闭，以兼容 WebSocket 等）。
        #[arg(long)]
        http2: bool,
        /// Web 管理界面端口。
        #[arg(long, default_value_t = whistle_core::config::DEFAULT_UI_PORT)]
        ui_port: u16,
        /// 不启动 Web 管理界面。
        #[arg(long)]
        no_ui: bool,
        /// UI 模式：native（默认精简界面）或 whistle（内嵌 whistle 原生前端，实验性）。
        #[arg(long, default_value = "native")]
        ui: String,
    },
    /// 停止代理服务。
    Stop {
        /// 数据目录（CA、配置）；默认 ~/.whistle-rs。
        #[arg(long)]
        data_dir: Option<String>,
    },
    /// 查看代理服务状态。
    Status {
        /// 数据目录（CA、配置）；默认 ~/.whistle-rs。
        #[arg(long)]
        data_dir: Option<String>,
    },
    /// 管理根证书（CA）。
    Ca {
        /// 数据目录（CA、配置）；默认 ~/.whistle-rs。
        #[arg(long, global = true)]
        data_dir: Option<String>,
        #[command(subcommand)]
        action: CaAction,
    },
}

#[derive(Debug, Subcommand)]
enum CaAction {
    /// 导出根证书到文件（不存在则先生成）。
    Export {
        /// 输出路径。
        #[arg(default_value = "whistle-rs-ca.pem")]
        path: String,
    },
    /// 打印根证书数据目录与路径。
    Path,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Start {
            config: config_path,
            host,
            port,
            rules,
            data_dir,
            no_decrypt,
            http2,
            ui_port,
            no_ui,
            ui,
        } => {
            // 提供 --config 时优先使用配置文件，忽略其它 start 参数。
            let config = match config_path {
                Some(path) => {
                    let cfg = Config::from_toml_file(&path).map_err(anyhow::Error::msg)?;
                    tracing::info!(%path, "已从配置文件加载");
                    cfg
                }
                None => Config {
                    host: host.unwrap_or_else(|| Config::default().host),
                    port,
                    rules_file: rules,
                    data_dir,
                    decrypt_https: !no_decrypt,
                    enable_http2: http2,
                    ui_port,
                    ui_enabled: !no_ui,
                    ui_mode: ui,
                    ..Config::default()
                },
            };

            // 启动前写入 PID 文件，供 stop/status 子命令读取。
            let dir = whistle_core::data_dir(&config);
            std::fs::create_dir_all(&dir)?;
            let pid_path = pid_file_path(&dir);
            if let Err(err) =
                write_pid_file(&pid_path, std::process::id(), config.port, config.ui_port)
            {
                tracing::warn!(error = %err, "写入 PID 文件失败");
            }

            let result = whistle_core::start(config).await;

            // 退出后尽力清理 PID 文件，忽略错误。
            let _ = std::fs::remove_file(&pid_path);

            result?;
        }
        Command::Stop { data_dir } => {
            let config = Config {
                data_dir,
                ..Config::default()
            };
            let pid_path = pid_file_path(&whistle_core::data_dir(&config));
            match read_pid_file(&pid_path)? {
                None => println!("whistle-rs 未在运行"),
                Some(info) => {
                    stop_process(info.pid)?;
                    let _ = std::fs::remove_file(&pid_path);
                    println!("已停止 whistle-rs（pid {})", info.pid);
                }
            }
        }
        Command::Status { data_dir } => {
            let config = Config {
                data_dir,
                ..Config::default()
            };
            let pid_path = pid_file_path(&whistle_core::data_dir(&config));
            match read_pid_file(&pid_path)? {
                None => println!("未运行"),
                Some(info) => {
                    if process_alive(info.pid) {
                        println!(
                            "运行中（pid {}，代理端口 {}，Web 端口 {}）",
                            info.pid, info.port, info.ui_port
                        );
                    } else {
                        println!(
                            "未运行（PID 文件残留：pid {}，代理端口 {}，Web 端口 {}）",
                            info.pid, info.port, info.ui_port
                        );
                    }
                }
            }
        }
        Command::Ca { data_dir, action } => {
            let config = Config {
                data_dir,
                ..Config::default()
            };
            let ca = whistle_core::load_ca(&config)?;
            match action {
                CaAction::Export { path } => {
                    std::fs::write(&path, ca.ca_pem())?;
                    println!("已导出根证书到 {path}");
                    println!("在系统/浏览器中信任它后，即可解密 HTTPS 流量。");
                }
                CaAction::Path => {
                    let dir = whistle_core::data_dir(&config);
                    println!("数据目录: {}", dir.display());
                    println!("CA 证书:  {}", dir.join("ca-cert.pem").display());
                }
            }
        }
    }
    Ok(())
}

/// PID 文件名（位于数据目录下）。
const PID_FILE_NAME: &str = "whistle-rs.pid";

/// PID 文件中记录的运行信息。
struct PidInfo {
    /// 进程 PID。
    pid: u32,
    /// 代理监听端口。
    port: u16,
    /// Web 管理界面端口。
    ui_port: u16,
}

/// 返回数据目录下的 PID 文件路径。
fn pid_file_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(PID_FILE_NAME)
}

/// 写入 PID 文件（手写 JSON，避免引入 serde_json）。
fn write_pid_file(
    path: &std::path::Path,
    pid: u32,
    port: u16,
    ui_port: u16,
) -> std::io::Result<()> {
    let json = format!(r#"{{"pid":{pid},"port":{port},"ui_port":{ui_port}}}"#);
    std::fs::write(path, json)
}

/// 读取并解析 PID 文件；文件不存在或内容损坏时返回 `Ok(None)`。
fn read_pid_file(path: &std::path::Path) -> anyhow::Result<Option<PidInfo>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    Ok(parse_pid_file(&text))
}

/// 从 JSON 文本中提取 pid/port/ui_port。任一字段缺失/非法则返回 `None`。
fn parse_pid_file(text: &str) -> Option<PidInfo> {
    let pid = json_number_field(text, "pid")?;
    let port = json_number_field(text, "port")?;
    let ui_port = json_number_field(text, "ui_port")?;
    Some(PidInfo {
        pid: u32::try_from(pid).ok()?,
        port: u16::try_from(port).ok()?,
        ui_port: u16::try_from(ui_port).ok()?,
    })
}

/// 在简单 JSON 文本里查找 `"key":<number>` 并返回其整数值。
fn json_number_field(text: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\"");
    let start = text.find(&needle)? + needle.len();
    let rest = text[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// 终止指定进程。Unix 上发送 SIGTERM（`kill <pid>`）。
fn stop_process(pid: u32) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let status = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status()?;
        if !status.success() {
            anyhow::bail!("kill {pid} 失败（进程可能已退出）");
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("当前平台暂不支持停止进程（pid {pid}）")
    }
}

/// 判断进程是否仍在运行。Linux 上检查 `/proc/<pid>` 是否存在。
fn process_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        true
    }
}

/// 按 verbose 级别初始化日志；`RUST_LOG` 优先生效。
fn init_tracing(verbose: u8) {
    use tracing_subscriber::{fmt, EnvFilter};

    let default_level = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    fmt().with_env_filter(filter).init();
}
