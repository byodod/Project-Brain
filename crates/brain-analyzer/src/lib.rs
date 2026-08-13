use std::{collections::BTreeMap, path::Path};

pub use brain_symbols::SourceLanguage;
use brain_symbols::{
    EdgeKind, IdentityQuality, ProviderDescriptor, SymbolEdge, SymbolNode, SymbolNodeInput,
    encode_provider_key,
};
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileSymbolIndex {
    pub provider: ProviderDescriptor,
    pub language: SourceLanguage,
    pub has_syntax_errors: bool,
    pub symbols: Vec<SymbolNode>,
    pub edges: Vec<SymbolEdge>,
}

pub fn rust_syntax_provider() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "tree-sitter-rust-syntax".to_owned(),
        version: concat!(env!("CARGO_PKG_VERSION"), "+tree-sitter-rust-0.24.2").to_owned(),
        identity_quality: IdentityQuality::SyntaxFallback,
    }
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

/// 为单个文件生成 provider-neutral 符号节点和确定性的词法包含边。
///
/// 该 Provider 明确标记为 `syntax_fallback`；输出 ID 只保证相同路径、种类和
/// 限定名下可重复，不承诺 rename/move 后保持身份。
///
/// # Errors
///
/// Tree-sitter grammar 无法加载或解析器未产生语法树时返回错误。
pub fn index_file_symbols(
    path: &str,
    language: SourceLanguage,
    source: &str,
) -> Result<FileSymbolIndex, AnalyzerError> {
    let mut parser = configured_parser(language)?;
    let tree = parser
        .parse(source, None)
        .ok_or(AnalyzerError::ParseFailed)?;
    let root = tree.root_node();
    let provider = rust_syntax_provider();
    let mut symbols = Vec::new();
    let mut edges = Vec::new();
    collect_index_symbols(
        root,
        source,
        path,
        language,
        &provider,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut BTreeMap::new(),
        &mut symbols,
        &mut edges,
    );
    symbols.sort_by(|left, right| left.id.cmp(&right.id));
    edges.sort_by(|left, right| {
        (&left.source_id, &left.target_id, left.kind).cmp(&(
            &right.source_id,
            &right.target_id,
            right.kind,
        ))
    });
    Ok(FileSymbolIndex {
        provider,
        language,
        has_syntax_errors: root.has_error(),
        symbols,
        edges,
    })
}

fn configured_parser(language: SourceLanguage) -> Result<Parser, AnalyzerError> {
    let mut parser = Parser::new();
    match language {
        SourceLanguage::Rust => parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .map_err(|error| AnalyzerError::Grammar {
                language,
                message: error.to_string(),
            })?,
    }
    Ok(parser)
}

#[allow(clippy::too_many_arguments)]
fn collect_index_symbols(
    node: Node<'_>,
    source: &str,
    path: &str,
    language: SourceLanguage,
    provider: &ProviderDescriptor,
    owner_names: &mut Vec<String>,
    owner_ids: &mut Vec<String>,
    occurrences: &mut BTreeMap<String, usize>,
    symbols: &mut Vec<SymbolNode>,
    edges: &mut Vec<SymbolEdge>,
) {
    let mut pushed = false;
    if let Some((local_name, kind)) = symbol_identity(node, source) {
        let display_name = if owner_names.is_empty() {
            local_name.clone()
        } else {
            format!("{}::{local_name}", owner_names.join("::"))
        };
        let identity_key = encode_provider_key(&[path, kind, &display_name]);
        let occurrence = occurrences.entry(identity_key.clone()).or_default();
        let provider_key = encode_provider_key(&[&identity_key, &occurrence.to_string()]);
        *occurrence += 1;
        let content = source
            .as_bytes()
            .get(node.start_byte()..node.end_byte())
            .unwrap_or_default();
        let symbol = SymbolNode::from_provider_key(
            provider,
            SymbolNodeInput {
                language,
                kind,
                provider_key: &provider_key,
                display_name: &display_name,
                path,
                start_line: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                content,
            },
        );
        if let Some(parent_id) = owner_ids.last() {
            edges.push(SymbolEdge {
                provider_id: provider.id.clone(),
                source_id: parent_id.clone(),
                target_id: symbol.id.clone(),
                kind: EdgeKind::Contains,
            });
        }
        owner_names.push(local_name);
        owner_ids.push(symbol.id.clone());
        symbols.push(symbol);
        pushed = true;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_index_symbols(
            child,
            source,
            path,
            language,
            provider,
            owner_names,
            owner_ids,
            occurrences,
            symbols,
            edges,
        );
    }
    if pushed {
        owner_names.pop();
        owner_ids.pop();
    }
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
        "impl_item" => {
            let implementation = node.child_by_field_name("type")?;
            let implementation = implementation.utf8_text(source.as_bytes()).ok()?.trim();
            let name = if let Some(trait_node) = node.child_by_field_name("trait") {
                let trait_name = trait_node.utf8_text(source.as_bytes()).ok()?.trim();
                format!("impl {trait_name} for {implementation}")
            } else {
                format!("impl {implementation}")
            };
            return Some((name, kind));
        }
        _ => return None,
    }?;
    let raw_name = name_node.utf8_text(source.as_bytes()).ok()?.trim();
    if raw_name.is_empty() {
        return None;
    }
    Some((raw_name.to_owned(), kind))
}

fn qualified_name_component(node: Node<'_>, source: &str) -> String {
    symbol_identity(node, source)
        .map(|(name, _)| name)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        LineRange, SourceLanguage, analyze_changed_symbols, detect_language, index_file_symbols,
    };
    use brain_symbols::{EdgeKind, IdentityQuality};

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

    #[test]
    fn index_marks_syntax_identity_and_emits_contains_edges() {
        let source = "struct Worker;\nimpl Worker { fn run(&self) {} }\n";
        let index = index_file_symbols("src/lib.rs", SourceLanguage::Rust, source).unwrap();
        assert_eq!(
            index.provider.identity_quality,
            IdentityQuality::SyntaxFallback
        );
        assert!(
            index
                .symbols
                .iter()
                .any(|symbol| symbol.display_name == "impl Worker::run")
        );
        assert!(
            index
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::Contains)
        );
    }

    #[test]
    fn trait_impls_have_distinct_fallback_owners() {
        let source = "trait A { fn run(&self); }\ntrait B { fn run(&self); }\nstruct Worker;\nimpl A for Worker { fn run(&self) {} }\nimpl B for Worker { fn run(&self) {} }\n";
        let index = index_file_symbols("src/lib.rs", SourceLanguage::Rust, source).unwrap();
        assert!(
            index
                .symbols
                .iter()
                .any(|symbol| symbol.display_name == "impl A for Worker::run")
        );
        assert!(
            index
                .symbols
                .iter()
                .any(|symbol| symbol.display_name == "impl B for Worker::run")
        );
    }

    #[test]
    fn multiple_inherent_impl_blocks_receive_distinct_fallback_ids() {
        let source = "struct Worker;\nimpl Worker { fn first(&self) {} }\nimpl Worker { fn second(&self) {} }\n";
        let index = index_file_symbols("src/lib.rs", SourceLanguage::Rust, source).unwrap();
        let impls = index
            .symbols
            .iter()
            .filter(|symbol| symbol.display_name == "impl Worker")
            .collect::<Vec<_>>();
        assert_eq!(impls.len(), 2);
        assert_ne!(impls[0].id, impls[1].id);
        assert!(
            index
                .symbols
                .iter()
                .all(|symbol| !symbol.provider_key.contains('\0'))
        );
    }
}
