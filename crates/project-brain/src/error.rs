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

    #[error("找不到 Project Brain 配置；请先在项目根目录执行 project-brain init")]
    ProjectNotInitialized,

    #[error("Project Brain 已初始化：{0}")]
    AlreadyInitialized(PathBuf),

    #[error("Git 命令失败：{0}")]
    Git(String),
}
