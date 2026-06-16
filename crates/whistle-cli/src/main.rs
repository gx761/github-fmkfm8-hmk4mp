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
    },
    /// 停止代理服务。
    Stop,
    /// 查看代理服务状态。
    Status,
    /// 管理根证书（CA）。
    Ca {
        #[command(subcommand)]
        action: CaAction,
    },
}

#[derive(Debug, Subcommand)]
enum CaAction {
    /// 导出根证书到文件。
    Export {
        /// 输出路径。
        #[arg(default_value = "whistle-rs-ca.pem")]
        path: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Start { port } => {
            let config = Config {
                port,
                ..Config::default()
            };
            whistle_core::start(config).await?;
        }
        Command::Stop => tracing::warn!("stop 尚未实现（需进程间通信，计划于后续里程碑）"),
        Command::Status => tracing::warn!("status 尚未实现（需进程间通信，计划于后续里程碑）"),
        Command::Ca { action } => match action {
            CaAction::Export { path } => {
                tracing::warn!(%path, "ca export 尚未实现（计划于 M3）")
            }
        },
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
