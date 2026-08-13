mod analyze;
mod app;
mod codex;
mod error;
mod git;
mod index;
mod protocol;
mod provider;
mod reconcile;
mod scip_index;
mod setup;

use std::{path::PathBuf, process::ExitCode};

use app::{AgentKind, App, HookEvent, ProjectProfile};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "project-brain")]
#[command(about = "由项目决策记忆驱动的确定性 Agent 控制面")]
#[command(version)]
struct Cli {
    /// 项目根目录；省略时从当前目录向上查找 .project-brain/config.json
    #[arg(long, global = true)]
    project_root: Option<PathBuf>,

    /// 机器级安装根；主要用于测试、便携安装和管理员部署
    #[arg(long, global = true)]
    install_root: Option<PathBuf>,

    /// Codex 配置根；省略时使用 `CODEX_HOME` 或 `~/.codex`
    #[arg(long, global = true)]
    codex_home: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 把当前版本安装到机器级稳定 launcher 与版本化 payload 目录
    Install,

    /// 原子切回安装清单中的上一版本，不修改 Hook 定义
    Rollback,

    /// 初始化仓库级 Project Brain 配置；语言能力只能通过显式 profile 声明
    Init {
        /// 可重复指定 rust、dotnet、python；省略时创建不含语言假设的基础配置
        #[arg(long, value_enum)]
        profile: Vec<ProjectProfile>,
    },

    /// 将已初始化项目注册到当前机器，并可安装 Codex 用户级 dispatcher
    Bootstrap {
        #[arg(long)]
        codex: bool,
    },

    /// 安装用户级 Agent lifecycle dispatcher
    InstallHooks { agent: AgentKind },

    /// 只移除 Project Brain 管理的用户级 lifecycle dispatcher
    UninstallHooks {
        agent: AgentKind,
        #[arg(long)]
        force: bool,
    },

    /// 用户级 Hook 入口；未注册项目静默 NO-OP
    Dispatch { agent: AgentKind, event: HookEvent },

    /// 检查安装、项目注册、Codex Hook 与本地存储就绪状态
    Doctor,

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

    /// 从已有 .scip 文件按项目 provider profile 离线建立 semantic 快照
    IndexScip {
        /// `.project-brain/config.json` 中的 semantic provider profile ID
        #[arg(long)]
        provider: String,

        #[arg(long)]
        input: PathBuf,
    },

    /// 管理当前项目的机器级语义 Provider 绑定与安全索引执行
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },

    /// 查看或显式裁决 semantic lineage 候选
    Lineage {
        #[command(subcommand)]
        command: LineageCommand,
    },

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

#[derive(Debug, Subcommand)]
enum ProviderCommand {
    /// 将仓库 profile 绑定到本机可执行文件；路径只写入机器状态
    Bind {
        #[arg(long)]
        profile: String,
        #[arg(long)]
        executable: PathBuf,
        /// 可选的机器级 launcher 脚本，例如由 node.exe 直接加载的 scip-python JS 入口
        #[arg(long)]
        script: Option<PathBuf>,
        /// 显式替换已有且不同的绑定
        #[arg(long)]
        replace: bool,
        /// 确认信任此机器本地 executable/entrypoint；Hook 不会自动提供此参数
        #[arg(long)]
        trust_local_executable: bool,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=120))]
        timeout_seconds: u64,
    },

    /// 删除当前项目的一个机器级 Provider 绑定
    Unbind {
        #[arg(long)]
        profile: String,
    },

    /// 查看仓库 profile 与当前机器绑定状态
    List,

    /// 在隔离临时目录运行已绑定 Provider，并事务化导入生成的 SCIP
    Index {
        #[arg(long)]
        profile: String,
        #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
    },
}

#[derive(Debug, Subcommand)]
enum LineageCommand {
    /// 查询项目级 lineage 候选 ledger
    Candidates {
        #[arg(long)]
        state: Option<LineageStateArg>,

        #[arg(long)]
        snapshot: Option<String>,

        #[arg(long)]
        ambiguity_group: Option<String>,

        #[arg(long, default_value_t = 200)]
        limit: u32,
    },

    /// 显式确认候选；可原子替代一条旧确认
    Confirm {
        #[arg(long)]
        candidate: String,

        #[arg(long)]
        request_id: String,

        #[arg(long)]
        actor_ref: Option<String>,

        #[arg(long)]
        reason: Option<String>,

        #[arg(long)]
        supersede: Option<String>,
    },

    /// 显式拒绝尚未裁决的候选
    Reject {
        #[arg(long)]
        candidate: String,

        #[arg(long)]
        request_id: String,

        #[arg(long)]
        actor_ref: Option<String>,

        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LineageStateArg {
    Proposed,
    Confirmed,
    Rejected,
    Superseded,
    Invalidated,
}

impl From<LineageStateArg> for brain_symbols::LineageState {
    fn from(value: LineageStateArg) -> Self {
        match value {
            LineageStateArg::Proposed => Self::Proposed,
            LineageStateArg::Confirmed => Self::Confirmed,
            LineageStateArg::Rejected => Self::Rejected,
            LineageStateArg::Superseded => Self::Superseded,
            LineageStateArg::Invalidated => Self::Invalidated,
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "CLI 顶层保持所有子命令到 App 的显式、可审计路由"
)]
fn main() -> ExitCode {
    match setup::delegate_if_installed_launcher() {
        Ok(Some(exit_code)) => return exit_code,
        Ok(None) => {}
        Err(error) => {
            eprintln!("project-brain: {error}");
            return ExitCode::FAILURE;
        }
    }
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Install => App::install_machine(cli.install_root.as_deref()),
        Command::Rollback => App::rollback_machine(cli.install_root.as_deref()),
        Command::Init { profile } => App::init(cli.project_root, &profile),
        Command::Bootstrap { codex } => App::open(cli.project_root).and_then(|app| {
            app.bootstrap(
                cli.install_root.as_deref(),
                cli.codex_home.as_deref(),
                codex,
            )
        }),
        Command::InstallHooks { agent } => App::install_hooks(
            cli.install_root.as_deref(),
            cli.codex_home.as_deref(),
            agent,
        ),
        Command::UninstallHooks { agent, force } => App::uninstall_hooks(
            cli.install_root.as_deref(),
            cli.codex_home.as_deref(),
            agent,
            force,
        ),
        Command::Dispatch { agent, event } => {
            App::dispatch_hook(cli.install_root.as_deref(), agent, event)
        }
        Command::Doctor => App::open(cli.project_root)
            .and_then(|app| app.doctor(cli.install_root.as_deref(), cli.codex_home.as_deref())),
        Command::Preflight => App::open(cli.project_root).and_then(|app| app.preflight()),
        Command::Hook { agent, event } => App::run_hook(cli.project_root, agent, event),
        Command::Capabilities { agent } => App::capabilities(agent),
        Command::Reconcile { base, envelope } => {
            App::open(cli.project_root).and_then(|app| app.reconcile(&base, &envelope))
        }
        Command::Analyze { base } => App::open(cli.project_root).and_then(|app| app.analyze(&base)),
        Command::Index => App::open(cli.project_root).and_then(|app| app.index()),
        Command::IndexScip { provider, input } => {
            App::open(cli.project_root).and_then(|app| app.index_scip(&provider, &input))
        }
        Command::Provider { command } => {
            App::open(cli.project_root).and_then(|app| match command {
                ProviderCommand::Bind {
                    profile,
                    executable,
                    script,
                    replace,
                    trust_local_executable,
                    timeout_seconds,
                } => app.bind_provider(
                    cli.install_root.as_deref(),
                    &profile,
                    &executable,
                    script.as_deref(),
                    replace,
                    trust_local_executable,
                    timeout_seconds,
                ),
                ProviderCommand::Unbind { profile } => {
                    app.unbind_provider(cli.install_root.as_deref(), &profile)
                }
                ProviderCommand::List => app.list_providers(cli.install_root.as_deref()),
                ProviderCommand::Index {
                    profile,
                    timeout_seconds,
                } => {
                    app.index_with_provider(cli.install_root.as_deref(), &profile, timeout_seconds)
                }
            })
        }
        Command::Lineage { command } => App::open(cli.project_root).and_then(|app| match command {
            LineageCommand::Candidates {
                state,
                snapshot,
                ambiguity_group,
                limit,
            } => app.lineage_candidates(
                state.map(Into::into),
                snapshot.as_deref(),
                ambiguity_group.as_deref(),
                limit,
            ),
            LineageCommand::Confirm {
                candidate,
                request_id,
                actor_ref,
                reason,
                supersede,
            } => app.confirm_lineage(
                &candidate,
                &request_id,
                actor_ref.as_deref(),
                reason.as_deref(),
                supersede.as_deref(),
            ),
            LineageCommand::Reject {
                candidate,
                request_id,
                actor_ref,
                reason,
            } => app.reject_lineage(
                &candidate,
                &request_id,
                actor_ref.as_deref(),
                reason.as_deref(),
            ),
        }),
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
