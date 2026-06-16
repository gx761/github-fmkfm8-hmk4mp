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
    },
    /// 停止代理服务。
    Stop,
    /// 查看代理服务状态。
    Status,
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
            port,
            rules,
            data_dir,
            no_decrypt,
        } => {
            let config = Config {
                port,
                rules_file: rules,
                data_dir,
                decrypt_https: !no_decrypt,
                ..Config::default()
            };
            whistle_core::start(config).await?;
        }
        Command::Stop => tracing::warn!("stop 尚未实现（需进程间通信，计划于后续里程碑）"),
        Command::Status => tracing::warn!("status 尚未实现（需进程间通信，计划于后续里程碑）"),
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
