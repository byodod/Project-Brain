mod analyze;
mod app;
mod artifact_store;
mod build;
mod claude;
mod codex;
mod error;
mod git;
mod godot;
mod index;
mod prime;
mod protocol;
mod provider;
mod reconcile;
mod runtime;
mod scip_index;
mod setup;
mod test;

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

    /// Claude Code 配置根；省略时使用 `CLAUDE_CONFIG_DIR` 或 `~/.claude`
    #[arg(long, global = true)]
    claude_home: Option<PathBuf>,

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

    /// 检查安装、项目注册、指定 Agent Hook 与本地存储就绪状态
    Doctor {
        #[arg(value_enum, default_value = "codex")]
        agent: AgentKind,
    },

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

    /// 生成分层项目证据；不会导出或发布项目
    Evidence {
        #[command(subcommand)]
        command: EvidenceCommand,
    },

    /// 查看或显式裁决 semantic lineage 候选
    Lineage {
        #[command(subcommand)]
        command: LineageCommand,
    },

    /// 管理仓库规则的 semantic symbol scope
    Rules {
        #[command(subcommand)]
        command: RulesCommand,
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

    /// 重复运行 Provider 并比较完整文档集合与语义指纹；不会提交 semantic snapshot
    VerifyStability {
        #[arg(long)]
        profile: String,
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(2..=20))]
        runs: u8,
        #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
    },

    /// 检查最新语义快照对声明源码的实际覆盖；partial/stale 返回非零
    Coverage {
        /// 同时要求每个 profile 已索引且覆盖率可验证为 complete
        #[arg(long)]
        require_indexed: bool,
    },
}

#[derive(Debug, Subcommand)]
enum EvidenceCommand {
    /// 查看当前项目已持久化 Evidence heads 及 fresh/stale 状态
    Status,

    /// 使用锁定的 Godot 4 editor 实际导入并加载项目资源
    Godot {
        /// Godot 4 editor/console binary 的机器绝对路径
        #[arg(long)]
        executable: PathBuf,
        /// 确认信任此机器本地 executable；Hook 不会自动提供此参数
        #[arg(long)]
        trust_local_executable: bool,
        #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
    },

    /// 使用固定工具链合同生成 Build Evidence；不会运行测试、应用或导出
    Build {
        #[command(subcommand)]
        command: BuildEvidenceCommand,
    },

    /// 从已验证 Build bundle 的精确字节运行固定 Test 合同；不会构建、还原或导出
    Test {
        #[command(subcommand)]
        command: TestEvidenceCommand,
    },

    /// 从已验证 Build bundle 的精确字节运行隔离 Godot headless Runtime；绝不构建或导出
    Runtime {
        /// Build Evidence 中的 content-addressed `RuntimeArtifactBundle` fingerprint
        #[arg(long)]
        bundle: String,
        /// Godot 4 editor/console binary 的机器绝对路径
        #[arg(long)]
        executable: PathBuf,
        #[arg(long)]
        trust_local_executable: bool,
        /// headless 主场景最多处理的迭代帧数
        #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u32).range(1..=3600))]
        quit_after: u32,
        #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
    },
}

#[derive(Debug, Subcommand)]
enum BuildEvidenceCommand {
    /// 固定执行 dotnet build Debug；MSBuild 可能执行仓库控制的构建代码
    Dotnet {
        /// 项目内稳定 Build profile ID；参与 Evidence head 身份
        #[arg(long)]
        profile: String,
        #[arg(long)]
        executable: PathBuf,
        /// 项目内单个 .csproj 路径；v1 不接受多项目 .sln
        #[arg(long)]
        target: PathBuf,
        /// Godot C# 等项目要求引用当前 fresh Engine Evidence
        #[arg(long)]
        require_engine: bool,
        #[arg(long)]
        trust_local_executable: bool,
        #[arg(long)]
        trust_repository_build_code: bool,
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
    },

    /// 固定执行 cargo build --workspace --all-targets --frozen；Cargo 可能执行 build.rs
    Rust {
        /// 项目内稳定 Build profile ID；参与 Evidence head 身份
        #[arg(long)]
        profile: String,
        #[arg(long)]
        executable: PathBuf,
        /// 项目内 Cargo.toml 路径
        #[arg(long, default_value = "Cargo.toml")]
        manifest: PathBuf,
        #[arg(long)]
        trust_local_executable: bool,
        #[arg(long)]
        trust_repository_build_code: bool,
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
    },

    /// 使用 Python isolated mode 逐文件 compile；不 import、不 exec 项目模块、不构建 wheel
    Python {
        /// 项目内稳定 Build profile ID；参与 Evidence head 身份
        #[arg(long)]
        profile: String,
        #[arg(long)]
        executable: PathBuf,
        /// 项目内 Python 源码根目录
        #[arg(long, default_value = ".")]
        source_root: PathBuf,
        #[arg(long)]
        trust_local_executable: bool,
        #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
    },
}

#[derive(Debug, Subcommand)]
enum TestEvidenceCommand {
    /// 固定执行 dotnet vstest；只运行 Build CAS 中已验证的测试程序集
    Dotnet {
        #[arg(long)]
        profile: String,
        /// 必须对应当前 `dotnet-build.<profile>` Evidence head
        #[arg(long)]
        build_profile: String,
        #[arg(long)]
        executable: PathBuf,
        /// 与 Build bundle 绑定的项目内 .csproj
        #[arg(long)]
        target: PathBuf,
        /// Build bundle 内的测试程序集相对路径，例如 Game.Tests.dll
        #[arg(long)]
        test_assembly: PathBuf,
        #[arg(long)]
        trust_local_executable: bool,
        #[arg(long)]
        trust_repository_test_code: bool,
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
    },

    /// 固定执行 cargo test --workspace --all-targets --frozen；会执行 build.rs、proc macro 与测试代码
    Rust {
        #[arg(long)]
        profile: String,
        /// 必须对应当前 `cargo-build.<profile>` Evidence head
        #[arg(long)]
        build_profile: String,
        #[arg(long)]
        executable: PathBuf,
        /// 项目内 Cargo.toml 路径
        #[arg(long, default_value = "Cargo.toml")]
        manifest: PathBuf,
        #[arg(long)]
        trust_local_executable: bool,
        #[arg(long)]
        trust_repository_test_code: bool,
        #[arg(long, default_value_t = 900, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
    },

    /// 在物理 Source staging 中执行 adapter-owned Python manifest runner；不使用 pytest/discovery/plugin
    Python {
        #[arg(long)]
        profile: String,
        /// 必须对应当前 `python-compile.<profile>` Evidence head
        #[arg(long)]
        build_profile: String,
        #[arg(long)]
        executable: PathBuf,
        /// 与 Python Build Evidence 绑定的项目内源码根目录
        #[arg(long, default_value = ".")]
        source_root: PathBuf,
        /// `source_root` 内由仓库声明 module/function 的受限 JSON 清单
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        trust_local_executable: bool,
        #[arg(long)]
        trust_repository_test_code: bool,
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
    },

    /// 固定运行仓库内 Godot .tscn，并读取受限结构化断言结果；不会构建、还原或导出
    Godot {
        #[arg(long)]
        profile: String,
        /// 必须对应当前 `dotnet-build.<profile>` Evidence head
        #[arg(long)]
        build_profile: String,
        /// Godot 4 editor/console binary 的机器绝对路径
        #[arg(long)]
        executable: PathBuf,
        /// 与 Build bundle 绑定的项目内 .csproj
        #[arg(long)]
        target: PathBuf,
        /// 项目内单个 .tscn 场景；场景必须写出固定的结构化结果文件
        #[arg(long)]
        scenario: PathBuf,
        #[arg(long)]
        trust_local_executable: bool,
        #[arg(long)]
        trust_repository_test_code: bool,
        /// headless 测试场景最多处理的迭代帧数
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u32).range(1..=36000))]
        quit_after: u32,
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
    },
}

#[derive(Debug, Subcommand)]
enum LineageCommand {
    /// 预演或显式执行 V7 pair-first 歧义候选压缩；默认只读
    CompactLegacyProposals {
        /// 执行已预演的压缩；省略时绝不写数据库
        #[arg(long)]
        apply: bool,

        /// apply 模式必填，用于幂等重放与碰撞检测
        #[arg(long)]
        request_id: Option<String>,

        /// 确认本次逻辑删除来自显式人工决定
        #[arg(long)]
        human_confirmed: bool,
    },

    /// 查询 group-first lineage 摘要，不展开潜在笛卡尔积
    Groups {
        #[arg(long, default_value_t = 200)]
        limit: u32,
    },

    /// 查看一个 lineage group 的完整成员集合
    Group {
        #[arg(long)]
        group: String,
    },

    /// 从 ambiguity group 显式物化一个 proposed pair；不会自动确认
    Materialize {
        #[arg(long)]
        group: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        human_confirmed: bool,
    },

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

        /// 确认本次不可自动推导的裁决来自显式人工决定
        #[arg(long)]
        human_confirmed: bool,
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

        /// 确认本次不可自动推导的裁决来自显式人工决定
        #[arg(long)]
        human_confirmed: bool,
    },
}

#[derive(Debug, Subcommand)]
enum RulesCommand {
    /// 把一条仓库规则绑定到明确的 semantic snapshot/symbol 锚点
    BindSymbol {
        #[arg(long)]
        rule: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        contract: String,
        #[arg(long)]
        language: String,
        #[arg(long)]
        snapshot: String,
        #[arg(long)]
        symbol: String,
        /// 确认锚点选择来自显式人工决定
        #[arg(long)]
        human_confirmed: bool,
    },
    /// 删除一条规则上的精确 semantic symbol scope
    UnbindSymbol {
        #[arg(long)]
        rule: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        contract: String,
        #[arg(long)]
        language: String,
        #[arg(long)]
        snapshot: String,
        #[arg(long)]
        symbol: String,
        #[arg(long)]
        human_confirmed: bool,
    },
    /// 列出规则锚点及其当前 confirmed-lineage 解析结果
    SymbolScopes {
        #[arg(long)]
        rule: Option<String>,
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
        Command::InstallHooks { agent } => {
            let agent_home = match agent {
                AgentKind::Codex => cli.codex_home.as_deref(),
                AgentKind::ClaudeCode => cli.claude_home.as_deref(),
                AgentKind::PrimeAgent => None,
            };
            App::install_hooks(cli.install_root.as_deref(), agent_home, agent)
        }
        Command::UninstallHooks { agent, force } => {
            let agent_home = match agent {
                AgentKind::Codex => cli.codex_home.as_deref(),
                AgentKind::ClaudeCode => cli.claude_home.as_deref(),
                AgentKind::PrimeAgent => None,
            };
            App::uninstall_hooks(cli.install_root.as_deref(), agent_home, agent, force)
        }
        Command::Dispatch { agent, event } => {
            App::dispatch_hook(cli.install_root.as_deref(), agent, event)
        }
        Command::Doctor { agent } => {
            let agent_home = match agent {
                AgentKind::Codex => cli.codex_home.as_deref(),
                AgentKind::ClaudeCode => cli.claude_home.as_deref(),
                AgentKind::PrimeAgent => None,
            };
            App::open(cli.project_root)
                .and_then(|app| app.doctor(cli.install_root.as_deref(), agent, agent_home))
        }
        Command::Preflight => App::open(cli.project_root).and_then(|app| app.preflight()),
        Command::Hook { agent, event } => {
            App::run_hook(cli.project_root, cli.install_root.as_deref(), agent, event)
        }
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
                ProviderCommand::VerifyStability {
                    profile,
                    runs,
                    timeout_seconds,
                } => app.verify_provider_stability(
                    cli.install_root.as_deref(),
                    &profile,
                    runs,
                    timeout_seconds,
                ),
                ProviderCommand::Coverage { require_indexed } => {
                    app.provider_coverage(require_indexed)
                }
            })
        }
        Command::Evidence { command } => {
            App::open(cli.project_root).and_then(|app| match command {
                EvidenceCommand::Status => app.evidence_status(),
                EvidenceCommand::Godot {
                    executable,
                    trust_local_executable,
                    timeout_seconds,
                } => app.evidence_godot(&executable, trust_local_executable, timeout_seconds),
                EvidenceCommand::Build { command } => match command {
                    BuildEvidenceCommand::Dotnet {
                        profile,
                        executable,
                        target,
                        require_engine,
                        trust_local_executable,
                        trust_repository_build_code,
                        timeout_seconds,
                    } => app.evidence_build_dotnet(
                        cli.install_root.as_deref(),
                        &executable,
                        &profile,
                        &target,
                        require_engine,
                        trust_local_executable,
                        trust_repository_build_code,
                        timeout_seconds,
                    ),
                    BuildEvidenceCommand::Rust {
                        profile,
                        executable,
                        manifest,
                        trust_local_executable,
                        trust_repository_build_code,
                        timeout_seconds,
                    } => app.evidence_build_rust(
                        cli.install_root.as_deref(),
                        &executable,
                        &profile,
                        &manifest,
                        trust_local_executable,
                        trust_repository_build_code,
                        timeout_seconds,
                    ),
                    BuildEvidenceCommand::Python {
                        profile,
                        executable,
                        source_root,
                        trust_local_executable,
                        timeout_seconds,
                    } => app.evidence_build_python(
                        cli.install_root.as_deref(),
                        &executable,
                        &profile,
                        &source_root,
                        trust_local_executable,
                        timeout_seconds,
                    ),
                },
                EvidenceCommand::Test { command } => match command {
                    TestEvidenceCommand::Dotnet {
                        profile,
                        build_profile,
                        executable,
                        target,
                        test_assembly,
                        trust_local_executable,
                        trust_repository_test_code,
                        timeout_seconds,
                    } => app.evidence_test_dotnet(
                        cli.install_root.as_deref(),
                        &executable,
                        &profile,
                        &build_profile,
                        &target,
                        &test_assembly,
                        trust_local_executable,
                        trust_repository_test_code,
                        timeout_seconds,
                    ),
                    TestEvidenceCommand::Rust {
                        profile,
                        build_profile,
                        executable,
                        manifest,
                        trust_local_executable,
                        trust_repository_test_code,
                        timeout_seconds,
                    } => app.evidence_test_rust(
                        &executable,
                        &profile,
                        &build_profile,
                        &manifest,
                        trust_local_executable,
                        trust_repository_test_code,
                        timeout_seconds,
                    ),
                    TestEvidenceCommand::Python {
                        profile,
                        build_profile,
                        executable,
                        source_root,
                        manifest,
                        trust_local_executable,
                        trust_repository_test_code,
                        timeout_seconds,
                    } => app.evidence_test_python(
                        &executable,
                        &profile,
                        &build_profile,
                        &source_root,
                        &manifest,
                        trust_local_executable,
                        trust_repository_test_code,
                        timeout_seconds,
                    ),
                    TestEvidenceCommand::Godot {
                        profile,
                        build_profile,
                        executable,
                        target,
                        scenario,
                        trust_local_executable,
                        trust_repository_test_code,
                        quit_after,
                        timeout_seconds,
                    } => app.evidence_test_godot(
                        cli.install_root.as_deref(),
                        &executable,
                        &profile,
                        &build_profile,
                        &target,
                        &scenario,
                        trust_local_executable,
                        trust_repository_test_code,
                        quit_after,
                        timeout_seconds,
                    ),
                },
                EvidenceCommand::Runtime {
                    bundle,
                    executable,
                    trust_local_executable,
                    quit_after,
                    timeout_seconds,
                } => app.evidence_runtime_godot(
                    cli.install_root.as_deref(),
                    &bundle,
                    &executable,
                    trust_local_executable,
                    quit_after,
                    timeout_seconds,
                ),
            })
        }
        Command::Lineage { command } => App::open(cli.project_root).and_then(|app| match command {
            LineageCommand::CompactLegacyProposals {
                apply,
                request_id,
                human_confirmed,
            } => {
                app.compact_legacy_lineage_proposals(apply, request_id.as_deref(), human_confirmed)
            }
            LineageCommand::Groups { limit } => app.lineage_groups(limit),
            LineageCommand::Group { group } => app.lineage_group(&group),
            LineageCommand::Materialize {
                group,
                from,
                to,
                human_confirmed,
            } => app.materialize_lineage_group_pair(&group, &from, &to, human_confirmed),
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
                human_confirmed,
            } => app.confirm_lineage(
                &candidate,
                &request_id,
                actor_ref.as_deref(),
                reason.as_deref(),
                supersede.as_deref(),
                human_confirmed,
            ),
            LineageCommand::Reject {
                candidate,
                request_id,
                actor_ref,
                reason,
                human_confirmed,
            } => app.reject_lineage(
                &candidate,
                &request_id,
                actor_ref.as_deref(),
                reason.as_deref(),
                human_confirmed,
            ),
        }),
        Command::Rules { command } => App::open(cli.project_root).and_then(|app| match command {
            RulesCommand::BindSymbol {
                rule,
                provider,
                contract,
                language,
                snapshot,
                symbol,
                human_confirmed,
            } => app.bind_rule_symbol(
                &rule,
                &provider,
                &contract,
                &language,
                &snapshot,
                &symbol,
                human_confirmed,
            ),
            RulesCommand::UnbindSymbol {
                rule,
                provider,
                contract,
                language,
                snapshot,
                symbol,
                human_confirmed,
            } => app.unbind_rule_symbol(
                &rule,
                &provider,
                &contract,
                &language,
                &snapshot,
                &symbol,
                human_confirmed,
            ),
            RulesCommand::SymbolScopes { rule } => app.rule_symbol_scopes(rule.as_deref()),
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
