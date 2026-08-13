use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tree_sitter::{Node, Parser};

#[derive(Debug, Error)]
pub enum AnalyzerError {
    #[error("无法加载 {language:?} Tree-sitter grammar：{message}")]
    Grammar {
        language: SourceLanguage,
        message: String,
    },
    #[error("Tree-sitter 未能生成语法树")]
    ParseFailed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceLanguage {
    Rust,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineRange {
    pub start_line: usize,
    pub end_line: usize,
}

impl LineRange {
    pub fn new(start_line: usize, end_line: usize) -> Self {
        Self {
            start_line,
            end_line,
        }
    }

    fn intersects(self, start_line: usize, end_line: usize) -> bool {
        self.start_line <= end_line && start_line <= self.end_line
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedSymbol {
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileAnalysis {
    pub language: SourceLanguage,
    pub has_syntax_errors: bool,
    pub symbols: Vec<ChangedSymbol>,
}

pub fn detect_language(path: &Path) -> Option<SourceLanguage> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| extension.eq_ignore_ascii_case("rs"))
        .map(|_| SourceLanguage::Rust)
}

/// 分析给定源码中与变更行相交的语义符号。
///
/// 返回叶级符号和其词法所有者。例如方法修改会同时报告 `impl Type`
/// 与 `impl Type::method`，使控制面既能按精确函数匹配，也能按类型聚合。
///
/// # Errors
///
/// Tree-sitter grammar 无法加载或解析器未产生语法树时返回错误。
pub fn analyze_changed_symbols(
    language: SourceLanguage,
    source: &str,
    changed_ranges: &[LineRange],
) -> Result<FileAnalysis, AnalyzerError> {
    let mut parser = Parser::new();
    match language {
        SourceLanguage::Rust => parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .map_err(|error| AnalyzerError::Grammar {
                language,
                message: error.to_string(),
            })?,
    }
    let tree = parser
        .parse(source, None)
        .ok_or(AnalyzerError::ParseFailed)?;
    let root = tree.root_node();
    let mut symbols = Vec::new();
    collect_symbols(root, source, changed_ranges, &mut Vec::new(), &mut symbols);
    symbols.sort_by(|left, right| {
        (left.start_line, left.end_line, &left.name).cmp(&(
            right.start_line,
            right.end_line,
            &right.name,
        ))
    });
    symbols.dedup();
    Ok(FileAnalysis {
        language,
        has_syntax_errors: root.has_error(),
        symbols,
    })
}

fn collect_symbols(
    node: Node<'_>,
    source: &str,
    ranges: &[LineRange],
    owners: &mut Vec<String>,
    symbols: &mut Vec<ChangedSymbol>,
) {
    let symbol = symbol_identity(node, source);
    let pushed = if let Some((name, kind)) = symbol {
        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;
        let qualified_name = if owners.is_empty() {
            name
        } else {
            format!("{}::{name}", owners.join("::"))
        };
        if ranges
            .iter()
            .any(|range| range.intersects(start_line, end_line))
        {
            symbols.push(ChangedSymbol {
                name: qualified_name.clone(),
                kind: kind.to_owned(),
                start_line,
                end_line,
            });
        }
        owners.push(qualified_name_component(node, source));
        true
    } else {
        false
    };

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_symbols(child, source, ranges, owners, symbols);
    }
    if pushed {
        owners.pop();
    }
}

fn symbol_identity(node: Node<'_>, source: &str) -> Option<(String, &'static str)> {
    let kind = node.kind();
    let name_node = match kind {
        "function_item"
        | "function_signature_item"
        | "struct_item"
        | "enum_item"
        | "trait_item"
        | "union_item"
        | "type_item"
        | "const_item"
        | "static_item"
        | "mod_item"
        | "macro_definition" => node.child_by_field_name("name"),
        "impl_item" => node.child_by_field_name("type"),
        _ => return None,
    }?;
    let raw_name = name_node.utf8_text(source.as_bytes()).ok()?.trim();
    if raw_name.is_empty() {
        return None;
    }
    let name = if kind == "impl_item" {
        format!("impl {raw_name}")
    } else {
        raw_name.to_owned()
    };
    Some((name, kind))
}

fn qualified_name_component(node: Node<'_>, source: &str) -> String {
    symbol_identity(node, source)
        .map(|(name, _)| name)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{LineRange, SourceLanguage, analyze_changed_symbols, detect_language};

    #[test]
    fn detects_rust_by_extension() {
        assert_eq!(
            detect_language(Path::new("src/lib.rs")),
            Some(SourceLanguage::Rust)
        );
        assert_eq!(detect_language(Path::new("README.md")), None);
    }

    #[test]
    fn reports_method_and_lexical_owner_for_changed_body() {
        let source = "struct Worker;\n\nimpl Worker {\n    fn run(&self) {\n        let ready = true;\n    }\n}\n";
        let analysis =
            analyze_changed_symbols(SourceLanguage::Rust, source, &[LineRange::new(5, 5)]).unwrap();
        assert!(
            analysis
                .symbols
                .iter()
                .any(|symbol| symbol.name == "impl Worker")
        );
        assert!(
            analysis
                .symbols
                .iter()
                .any(|symbol| symbol.name == "impl Worker::run")
        );
        assert!(!analysis.has_syntax_errors);
    }

    #[test]
    fn ignores_symbols_outside_changed_range() {
        let source = "fn first() {}\n\nfn second() {}\n";
        let analysis =
            analyze_changed_symbols(SourceLanguage::Rust, source, &[LineRange::new(1, 1)]).unwrap();
        assert_eq!(analysis.symbols.len(), 1);
        assert_eq!(analysis.symbols[0].name, "first");
    }

    #[test]
    fn exposes_recoverable_syntax_errors() {
        let analysis = analyze_changed_symbols(
            SourceLanguage::Rust,
            "fn broken( {\n",
            &[LineRange::new(1, 1)],
        )
        .unwrap();
        assert!(analysis.has_syntax_errors);
    }
}
