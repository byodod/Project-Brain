use std::{fs, path::Path};

use brain_analyzer::{
    ChangedSymbol, LineRange, SourceLanguage, analyze_changed_symbols, detect_language,
};
use brain_core::CURRENT_SCHEMA_VERSION;
use serde::Serialize;

use crate::{error::AppError, git};

#[derive(Debug, Serialize)]
pub struct AnalysisReport {
    pub schema_version: u32,
    pub base: String,
    pub files: Vec<AnalyzedFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsupported_files: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct AnalyzedFile {
    pub path: String,
    pub language: SourceLanguage,
    pub has_syntax_errors: bool,
    pub changed_ranges: Vec<LineRange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_ranges: Vec<LineRange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_symbols: Vec<ChangedSymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_symbols: Vec<ChangedSymbol>,
}

pub fn evaluate(root: &Path, base: &str) -> Result<AnalysisReport, AppError> {
    let changed_files = git::changed_files(root, base)?;
    let untracked = git::untracked_files(root)?;
    let mut files = Vec::new();
    let mut unsupported_files = Vec::new();

    for path in changed_files {
        let Some(language) = detect_language(Path::new(&path)) else {
            unsupported_files.push(path);
            continue;
        };
        let hunks = git::diff_hunks(root, base, &path)?;
        let current_path = root.join(Path::new(&path));
        let current_source = if current_path.is_file() {
            Some(fs::read_to_string(&current_path)?)
        } else {
            None
        };
        let changed_ranges = if untracked.contains(&path) {
            current_source
                .as_deref()
                .map_or_else(Vec::new, entire_source_range)
        } else {
            hunks.iter().filter_map(|hunk| hunk.new).collect()
        };
        let (changed_symbols, current_errors) = if let Some(source) = current_source {
            let analysis = analyze_changed_symbols(language.clone(), &source, &changed_ranges)?;
            (analysis.symbols, analysis.has_syntax_errors)
        } else {
            (Vec::new(), false)
        };

        let removed_ranges = hunks
            .iter()
            .filter(|hunk| hunk.new.is_none())
            .filter_map(|hunk| hunk.old)
            .collect::<Vec<_>>();
        let (removed_symbols, old_errors) = if removed_ranges.is_empty() {
            (Vec::new(), false)
        } else if let Some(source) = git::file_at_revision(root, base, &path)? {
            let analysis = analyze_changed_symbols(language.clone(), &source, &removed_ranges)?;
            (analysis.symbols, analysis.has_syntax_errors)
        } else {
            (Vec::new(), false)
        };

        files.push(AnalyzedFile {
            path,
            language,
            has_syntax_errors: current_errors || old_errors,
            changed_ranges,
            removed_ranges,
            changed_symbols,
            removed_symbols,
        });
    }

    Ok(AnalysisReport {
        schema_version: CURRENT_SCHEMA_VERSION,
        base: base.to_owned(),
        files,
        unsupported_files,
    })
}

fn entire_source_range(source: &str) -> Vec<LineRange> {
    let line_count = source.lines().count().max(1);
    vec![LineRange::new(1, line_count)]
}

#[cfg(test)]
mod tests {
    use super::entire_source_range;
    use brain_analyzer::LineRange;

    #[test]
    fn whole_file_range_is_never_empty() {
        assert_eq!(entire_source_range(""), vec![LineRange::new(1, 1)]);
        assert_eq!(
            entire_source_range("one\ntwo\n"),
            vec![LineRange::new(1, 2)]
        );
    }
}
