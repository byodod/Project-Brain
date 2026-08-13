use std::{
    fs,
    path::{Component, Path},
};

use brain_analyzer::{detect_language, index_file_symbols, rust_syntax_provider};
use brain_core::CURRENT_SCHEMA_VERSION;
use brain_store::BrainStore;
use brain_symbols::{GraphDelta, ProviderDescriptor, SourceFileState, SymbolSnapshot};
use serde::Serialize;

use crate::{error::AppError, git};

#[derive(Debug, Serialize)]
pub struct IndexReport {
    pub schema_version: u32,
    pub project_key: String,
    pub provider: ProviderDescriptor,
    pub source_revision: String,
    pub indexed_files: u64,
    pub indexed_symbols: u64,
    pub indexed_edges: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub syntax_error_files: Vec<String>,
    pub delta: GraphDelta,
}

pub fn evaluate(
    root: &Path,
    project_key: &str,
    store: &BrainStore,
) -> Result<IndexReport, AppError> {
    let provider = rust_syntax_provider();
    let head_revision = git::head_revision(root)?;
    let mut sources = Vec::new();
    let mut symbols = Vec::new();
    let mut edges = Vec::new();
    let mut indexed_files = 0_u64;
    let mut syntax_error_files = Vec::new();

    for path in git::repository_files(root)? {
        let Some(language) = detect_language(Path::new(&path)) else {
            continue;
        };
        let Some(source) = read_repository_source(root, &path)? else {
            continue;
        };
        let file_index = index_file_symbols(project_key, &path, language.clone(), &source)?;
        if file_index.provider.id != provider.id
            || file_index.provider.version != provider.version
            || file_index.provider.identity_quality != provider.identity_quality
        {
            return Err(AppError::Store(brain_store::StoreError::InvalidSnapshot(
                "单次索引混入了不同 Provider".to_owned(),
            )));
        }
        if file_index.has_syntax_errors {
            syntax_error_files.push(path.clone());
        }
        sources.push(SourceFileState::from_source(
            &path,
            language,
            source.as_bytes(),
            file_index.has_syntax_errors,
        ));
        indexed_files += 1;
        symbols.extend(file_index.symbols);
        edges.extend(file_index.edges);
    }

    let snapshot = SymbolSnapshot::for_worktree(
        project_key,
        provider.clone(),
        &head_revision,
        sources,
        symbols,
        edges,
    );
    let indexed_symbols = u64::try_from(snapshot.symbols.len()).unwrap_or(u64::MAX);
    let indexed_edges = u64::try_from(snapshot.edges.len()).unwrap_or(u64::MAX);
    let source_revision = snapshot.source_revision.clone();
    let delta = store.apply_symbol_snapshot(&snapshot)?;
    Ok(IndexReport {
        schema_version: CURRENT_SCHEMA_VERSION,
        project_key: project_key.to_owned(),
        provider,
        source_revision,
        indexed_files,
        indexed_symbols,
        indexed_edges,
        syntax_error_files,
        delta,
    })
}

fn read_repository_source(root: &Path, path: &str) -> Result<Option<String>, AppError> {
    let relative = Path::new(path);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AppError::RepositoryPathOutsideRoot(relative.to_owned()));
    }
    let root = root.canonicalize()?;
    let candidate = root.join(relative);
    let canonical = match candidate.canonicalize() {
        Ok(canonical) => canonical,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !canonical.starts_with(&root) {
        return Err(AppError::RepositoryPathOutsideRoot(canonical));
    }
    if !canonical.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&canonical)?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| AppError::NonUtf8Source(path.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::read_repository_source;
    use crate::error::AppError;

    fn test_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "project-brain-index-path-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn reads_only_utf8_files_inside_the_repository() {
        let root = test_root();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        assert_eq!(
            read_repository_source(&root, "src/lib.rs").unwrap(),
            Some("fn main() {}\n".to_owned())
        );
        assert!(matches!(
            read_repository_source(&root, "../outside.rs"),
            Err(AppError::RepositoryPathOutsideRoot(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
