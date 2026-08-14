use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("I/O 操作失败：{0}")]
    Io(#[from] std::io::Error),

    #[error("系统时间无效：{0}")]
    Clock(#[from] std::time::SystemTimeError),

    #[error("JSON 解析或序列化失败：{0}")]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Core(#[from] brain_core::CoreError),

    #[error(transparent)]
    Store(#[from] brain_store::StoreError),

    #[error(transparent)]
    Analyzer(#[from] brain_analyzer::AnalyzerError),

    #[error(transparent)]
    Scip(#[from] brain_scip::ScipError),

    #[error(transparent)]
    Evidence(#[from] brain_evidence::EvidenceError),

    #[error("找不到 Project Brain 配置；请先在项目根目录执行 project-brain init")]
    ProjectNotInitialized,

    #[error("Project Brain 已初始化：{0}")]
    AlreadyInitialized(PathBuf),

    #[error("Change Envelope 必须位于项目根目录内：{0}")]
    EnvelopeOutsideRoot(PathBuf),

    #[error("仓库路径解析后越出项目根目录：{0}")]
    RepositoryPathOutsideRoot(PathBuf),

    #[error("Git 返回了非 UTF-8 路径；当前协议拒绝有损转换")]
    NonUtf8GitPath,

    #[error("源码不是有效 UTF-8：{0}")]
    NonUtf8Source(String),

    #[error("SCIP 导入不符合当前项目 language profile：{0}")]
    ScipProfileMismatch(String),

    #[error("机器级安装或项目 bootstrap 失败：{0}")]
    Setup(String),

    #[error("语义 Provider 执行失败：{0}")]
    Provider(String),

    #[error("规则治理操作失败：{0}")]
    Governance(String),

    #[error("数据库维护操作失败：{0}")]
    DatabaseMaintenance(String),

    #[error("数据库原子替换仍被临时占用：{0}")]
    DatabaseSwapBusy(String),

    #[error("Production Qualification 失败：{0}")]
    Qualification(String),

    #[error("Production Qualification 账本失败：{0}")]
    QualificationDatabase(#[from] rusqlite::Error),

    #[error("Project Brain doctor 检查未通过：{0}")]
    DoctorDegraded(String),

    #[error("Codex Hook 集成发生漂移，拒绝覆盖：{0}")]
    IntegrationDrift(PathBuf),

    #[error("目标在写入期间被并发修改，拒绝覆盖：{0}")]
    ConcurrentModification(PathBuf),

    #[error("Git 命令失败：{0}")]
    Git(String),
}
