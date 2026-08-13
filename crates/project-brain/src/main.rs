mod analyze;
mod app;
mod codex;
mod error;
mod git;
mod index;
mod protocol;
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

    /// 输出指定 Agent adapter 的已确认治理能力
    Capabilities { agent: AgentKind },

    /// 对照 Change Envelope 检查当前 Git 变更范围
    Reconcile {
        #[arg(long, default_value = "HEAD")]
        base: String,

        #[arg(long, default_value = ".project-brain/envelope.json")]
        envelope: PathBuf,
    },

    /// 提取当前 Git 变更触及的源代码符号
    Analyze {
        #[arg(long, default_value = "HEAD")]
        base: String,
    },

    /// 为当前工作区建立完整的 Provider-neutral 符号图快照
    Index,

    /// 查询本地符号图
    Symbols {
        #[arg(long)]
        path: Option<String>,

        #[arg(long)]
        include_removed: bool,

        #[arg(long, default_value_t = 200)]
        limit: u32,
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
        Command::Hook { agent, event } => App::run_hook(cli.project_root, agent, event),
        Command::Capabilities { agent } => App::capabilities(agent),
        Command::Reconcile { base, envelope } => {
            App::open(cli.project_root).and_then(|app| app.reconcile(&base, &envelope))
        }
        Command::Analyze { base } => App::open(cli.project_root).and_then(|app| app.analyze(&base)),
        Command::Index => App::open(cli.project_root).and_then(|app| app.index()),
        Command::Symbols {
            path,
            include_removed,
            limit,
        } => App::open(cli.project_root)
            .and_then(|app| app.symbols(path.as_deref(), include_removed, limit)),
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
