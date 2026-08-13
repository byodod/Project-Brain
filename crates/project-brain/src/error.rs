use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("I/O 操作失败：{0}")]
    Io(#[from] std::io::Error),

    #[error("JSON 解析或序列化失败：{0}")]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Core(#[from] brain_core::CoreError),

    #[error(transparent)]
    Store(#[from] brain_store::StoreError),

    #[error(transparent)]
    Analyzer(#[from] brain_analyzer::AnalyzerError),

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

    #[error("Git 命令失败：{0}")]
    Git(String),
}
