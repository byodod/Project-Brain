mod app;
mod codex;
mod error;
mod reconcile;

use std::{path::PathBuf, process::ExitCode};

use app::{AgentKind, App, HookEvent};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "project-brain")]
#[command(about = "由项目决策记忆驱动的确定性 Agent 控制面")]
struct Cli {
    /// 项目根目录；省略时从当前目录向上查找 .project-brain/config.json
    #[arg(long, global = true)]
    project_root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 初始化仓库级 Project Brain 配置
    Init,

    /// 从标准输入读取 `ActionDescriptor` 并输出通用决策 JSON
    Preflight,

    /// 从标准输入读取 Agent Hook JSON 并输出对应 Hook 协议
    Hook { agent: AgentKind, event: HookEvent },

    /// 对照 Change Envelope 检查当前 Git 变更范围
    Reconcile {
        #[arg(long, default_value = "HEAD")]
        base: String,

        #[arg(long, default_value = ".project-brain/envelope.json")]
        envelope: PathBuf,
    },

    /// 输出最近的本地 Hook 审计记录
    Audit {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Init => App::init(cli.project_root),
        Command::Preflight => App::open(cli.project_root).and_then(|app| app.preflight()),
        Command::Hook { agent, event } => {
            App::open(cli.project_root).and_then(|app| app.hook(agent, event))
        }
        Command::Reconcile { base, envelope } => {
            App::open(cli.project_root).and_then(|app| app.reconcile(&base, &envelope))
        }
        Command::Audit { limit } => App::open(cli.project_root).and_then(|app| app.audit(limit)),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("project-brain: {error}");
            ExitCode::FAILURE
        }
    }
}
