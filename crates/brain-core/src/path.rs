pub fn normalize_project_path(path: &str) -> String {
    let mut normalized = path.trim().replace('\\', "/");
    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest.to_owned();
    }
    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }
    let normalized = normalized.trim_end_matches('/');
    if normalized == "." {
        String::new()
    } else {
        normalized.to_owned()
    }
}

pub fn path_has_prefix(path: &str, prefix: &str) -> bool {
    let path = normalize_project_path(path);
    let prefix = normalize_project_path(prefix);
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
}

#[cfg(test)]
mod tests {
    use super::{normalize_project_path, path_has_prefix};

    #[test]
    fn normalizes_windows_and_relative_paths() {
        assert_eq!(normalize_project_path(r".\src\\core\"), "src/core");
        assert_eq!(normalize_project_path("."), "");
        assert_eq!(normalize_project_path("./"), "");
        assert_eq!(normalize_project_path(r".\"), "");
    }

    #[test]
    fn prefix_matching_respects_path_boundaries() {
        assert!(path_has_prefix("src/core/mod.rs", "src/core"));
        assert!(path_has_prefix("src/core", "src/core"));
        assert!(!path_has_prefix("src/core-old/mod.rs", "src/core"));
        assert!(path_has_prefix("src/core/mod.rs", "."));
        assert!(path_has_prefix("README.md", "./"));
    }
}
