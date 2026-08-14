use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use brain_evidence::EvidenceSnapshot;
use brain_godot::{GodotEvidenceReport, GodotProbeResult, build_engine_evidence};
use serde::Serialize;

use crate::{
    error::AppError,
    provider::{self, ProcessResult},
};

const GODOT_RUN_SCHEMA_VERSION: u32 = 1;
const MAX_PROBE_RESULT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct GodotRunReport {
    schema_version: u32,
    project_key: String,
    engine_version: String,
    executable_sha256: String,
    import: GodotProcessSummary,
    probe: GodotProcessSummary,
    evidence: GodotEvidenceReport,
}

impl GodotRunReport {
    pub fn evidence_snapshot(&self) -> &EvidenceSnapshot {
        &self.evidence.snapshot
    }
}

#[derive(Debug, Serialize)]
struct GodotProcessSummary {
    duration_ms: u128,
    exit_code: Option<i32>,
    stdout_bytes: usize,
    stderr_bytes: usize,
    stdout_sha256: String,
    stderr_sha256: String,
}

/// 使用显式信任的 Godot 4 editor binary 导入项目并运行只读结构化探针。
///
/// # Errors
///
/// 当 executable 未获显式信任、位于项目内、不是 Godot 4、进程失败或超时、输出被截断、探针结果
/// 无效，或引擎解析期间权威源文件漂移时返回错误。
pub fn run(
    project_root: &Path,
    project_key: &str,
    executable: &Path,
    trust_local_executable: bool,
    timeout_seconds: u64,
) -> Result<GodotRunReport, AppError> {
    if !trust_local_executable {
        return Err(AppError::Provider(
            "运行 Godot Engine Evidence Provider 需要显式传入 --trust-local-executable".to_owned(),
        ));
    }
    let root = project_root.canonicalize()?;
    if !root.join("project.godot").is_file() {
        return Err(AppError::Provider(format!(
            "项目根缺少 project.godot：{}",
            root.display()
        )));
    }
    let executable = provider::pin_external_executable(&root, executable, "Godot executable")?;
    let timeout = Duration::from_secs(timeout_seconds);
    let probe_files = ProbeFiles::create()?;
    let environment = probe_files.environment();
    let engine_version = qualify_engine(&root, &executable, timeout, &environment)?;

    fs::write(&probe_files.script, GODOT_PROBE_SCRIPT.as_bytes())?;
    let root_arg = godot_cli_path(&root);
    let import_process = provider::run_process_with_environment(
        &executable.canonical_path,
        None,
        &[
            "--headless".to_owned(),
            "--no-header".to_owned(),
            "--path".to_owned(),
            root_arg.clone(),
            "--import".to_owned(),
        ],
        &probe_files.directory,
        Some(&root),
        timeout,
        &environment,
    )?;
    require_complete_success("Godot import", &import_process)?;
    let probe_process = provider::run_process_with_environment(
        &executable.canonical_path,
        None,
        &[
            "--headless".to_owned(),
            "--no-header".to_owned(),
            "--path".to_owned(),
            root_arg,
            "--script".to_owned(),
            godot_cli_path(&probe_files.script),
            "--".to_owned(),
            godot_cli_path(&probe_files.output),
        ],
        &probe_files.directory,
        Some(&root),
        timeout,
        &environment,
    )?;
    require_complete_success("Godot evidence probe", &probe_process)?;
    let metadata = fs::symlink_metadata(&probe_files.output).map_err(|error| {
        AppError::Provider(format!("Godot evidence probe 未生成结果文件：{error}"))
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AppError::Provider(
            "Godot evidence probe 结果不是受控普通文件".to_owned(),
        ));
    }
    if metadata.len() > MAX_PROBE_RESULT_BYTES {
        return Err(AppError::Provider(format!(
            "Godot evidence probe 结果超过 {MAX_PROBE_RESULT_BYTES} 字节上限"
        )));
    }
    let probe: GodotProbeResult = serde_json::from_slice(&fs::read(&probe_files.output)?)?;
    let diagnostics = extract_diagnostics(
        [&import_process, &probe_process],
        &root,
        &probe_files.directory,
    );
    if provider::hash_file(&executable.canonical_path)? != executable.sha256 {
        return Err(AppError::Provider(
            "Godot executable 在证据运行期间发生漂移".to_owned(),
        ));
    }
    let identity = format!("{engine_version}+sha256.{}", executable.sha256);
    let evidence = build_engine_evidence(&root, project_key, &identity, &probe, &diagnostics)?;
    Ok(GodotRunReport {
        schema_version: GODOT_RUN_SCHEMA_VERSION,
        project_key: project_key.to_owned(),
        engine_version,
        executable_sha256: executable.sha256,
        import: process_summary(&import_process),
        probe: process_summary(&probe_process),
        evidence,
    })
}

fn qualify_engine(
    root: &Path,
    executable: &provider::PinnedExternalExecutable,
    timeout: Duration,
    environment: &[(&str, &Path)],
) -> Result<String, AppError> {
    let version_process = provider::run_process_with_environment(
        &executable.canonical_path,
        None,
        &["--version".to_owned()],
        root,
        Some(root),
        timeout,
        environment,
    )?;
    require_complete_success("Godot version probe", &version_process)?;
    let engine_version = provider::version_text(&version_process)?;
    if engine_version.split('.').next() != Some("4") {
        return Err(AppError::Provider(format!(
            "Godot Engine Evidence Provider v1 只支持 Godot 4，实际版本={engine_version:?}"
        )));
    }
    let help_process = provider::run_process_with_environment(
        &executable.canonical_path,
        None,
        &["--help".to_owned()],
        root,
        Some(root),
        timeout,
        environment,
    )?;
    require_complete_success("Godot capability probe", &help_process)?;
    let help = format!(
        "{}{}",
        String::from_utf8_lossy(&help_process.stdout.bytes),
        String::from_utf8_lossy(&help_process.stderr.bytes)
    );
    for capability in ["Godot Engine", "--headless", "--script", "--import"] {
        if !help.contains(capability) {
            return Err(AppError::Provider(format!(
                "Godot capability probe 缺少 {capability:?}，拒绝把 executable 认定为兼容 editor"
            )));
        }
    }
    Ok(engine_version)
}

fn require_complete_success(stage: &str, process: &ProcessResult) -> Result<(), AppError> {
    if process.stdout.truncated || process.stderr.truncated {
        return Err(AppError::Provider(format!(
            "{stage} 输出超过捕获上限，不能声称 complete evidence；stdout_bytes={} stderr_bytes={}",
            process.stdout.total_bytes, process.stderr.total_bytes
        )));
    }
    if !process.status.success() {
        let stderr = String::from_utf8_lossy(&process.stderr.bytes);
        let excerpt = stderr.chars().take(4_096).collect::<String>();
        return Err(AppError::Provider(format!(
            "{stage} 失败，exit_code={:?} stdout_sha256={} stderr_sha256={} stderr_excerpt={excerpt:?}",
            process.status.code(),
            process.stdout.sha256,
            process.stderr.sha256
        )));
    }
    Ok(())
}

fn process_summary(process: &ProcessResult) -> GodotProcessSummary {
    GodotProcessSummary {
        duration_ms: process.duration.as_millis(),
        exit_code: process.status.code(),
        stdout_bytes: process.stdout.total_bytes,
        stderr_bytes: process.stderr.total_bytes,
        stdout_sha256: process.stdout.sha256.clone(),
        stderr_sha256: process.stderr.sha256.clone(),
    }
}

fn extract_diagnostics<const N: usize>(
    processes: [&ProcessResult; N],
    project_root: &Path,
    probe_root: &Path,
) -> Vec<String> {
    let mut diagnostics = Vec::new();
    for process in processes {
        for bytes in [&process.stdout.bytes, &process.stderr.bytes] {
            for line in String::from_utf8_lossy(bytes).lines() {
                let trimmed = line.trim();
                if trimmed.contains("ERROR:") || trimmed.starts_with("SCRIPT ERROR") {
                    let normalized = sanitize_diagnostic(trimmed, project_root, probe_root);
                    if !diagnostics.contains(&normalized) {
                        diagnostics.push(normalized);
                    }
                }
            }
        }
    }
    diagnostics.sort();
    diagnostics
}

fn sanitize_diagnostic(line: &str, project_root: &Path, probe_root: &Path) -> String {
    let mut value = line.replace(
        &project_root.to_string_lossy().replace('\\', "/"),
        "<project>",
    );
    value = value.replace(
        &project_root.to_string_lossy().replace('/', "\\"),
        "<project>",
    );
    value = value.replace(&probe_root.to_string_lossy().replace('\\', "/"), "<probe>");
    value.replace(&probe_root.to_string_lossy().replace('/', "\\"), "<probe>")
}

fn godot_cli_path(path: &Path) -> String {
    provider::provider_cli_path(path).replace('\\', "/")
}

struct ProbeFiles {
    directory: PathBuf,
    script: PathBuf,
    output: PathBuf,
    home: PathBuf,
    app_data: PathBuf,
    local_app_data: PathBuf,
    xdg_data: PathBuf,
    xdg_config: PathBuf,
    xdg_cache: PathBuf,
}

impl ProbeFiles {
    fn create() -> Result<Self, AppError> {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let base = std::env::temp_dir();
        for attempt in 0_u16..100 {
            let directory = base.join(format!(
                "project-brain-godot-probe-{}-{timestamp}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    let home = directory.join("user/home");
                    let app_data = directory.join("user/appdata");
                    let local_app_data = directory.join("user/localappdata");
                    let xdg_data = directory.join("user/xdg-data");
                    let xdg_config = directory.join("user/xdg-config");
                    let xdg_cache = directory.join("user/xdg-cache");
                    for path in [
                        &home,
                        &app_data,
                        &local_app_data,
                        &xdg_data,
                        &xdg_config,
                        &xdg_cache,
                    ] {
                        fs::create_dir_all(path)?;
                    }
                    return Ok(Self {
                        script: directory.join("project_brain_probe.gd"),
                        output: directory.join("result.json"),
                        directory,
                        home,
                        app_data,
                        local_app_data,
                        xdg_data,
                        xdg_config,
                        xdg_cache,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(AppError::Provider(
            "无法创建唯一 Godot probe 临时目录".to_owned(),
        ))
    }

    fn environment(&self) -> [(&str, &Path); 6] {
        [
            ("HOME", &self.home),
            ("APPDATA", &self.app_data),
            ("LOCALAPPDATA", &self.local_app_data),
            ("XDG_DATA_HOME", &self.xdg_data),
            ("XDG_CONFIG_HOME", &self.xdg_config),
            ("XDG_CACHE_HOME", &self.xdg_cache),
        ]
    }
}

impl Drop for ProbeFiles {
    fn drop(&mut self) {
        let temp_root = std::env::temp_dir();
        if self.directory.starts_with(&temp_root) && self.directory != temp_root {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

const GODOT_PROBE_SCRIPT: &str = r#"extends SceneTree

const SCHEMA_VERSION := 1

func _init() -> void:
	var args := OS.get_cmdline_user_args()
	if args.size() != 1:
		quit(90)
		return
	var paths: Array[String] = []
	collect_resources("res://", paths)
	paths.sort()
	var before := capture_state(paths, {})
	var loaded := load_resources(paths)
	var after := capture_state(paths, loaded)
	var output := FileAccess.open(args[0], FileAccess.WRITE)
	if output == null:
		quit(91)
		return
	output.store_string(JSON.stringify({
		"schema_version": SCHEMA_VERSION,
		"before": before,
		"after": after,
	}))
	output.close()
	quit(0)

func collect_resources(directory_path: String, paths: Array[String]) -> void:
	var directory := DirAccess.open(directory_path)
	if directory == null:
		return
	directory.list_dir_begin()
	var name := directory.get_next()
	while name != "":
		var child := directory_path.path_join(name)
		if directory.current_is_dir():
			if not (directory_path == "res://" and name in [".godot", ".git"]):
				collect_resources(child, paths)
		elif name.get_extension().to_lower() in ["tscn", "tres"]:
			paths.append(child)
		name = directory.get_next()
	directory.list_dir_end()

func load_resources(paths: Array[String]) -> Dictionary:
	var loaded := {}
	for path in paths:
		loaded[path] = ResourceLoader.load(path, "", ResourceLoader.CACHE_MODE_IGNORE) != null
	return loaded

func capture_state(paths: Array[String], loaded: Dictionary) -> Dictionary:
	var resources: Array[Dictionary] = []
	for path in paths:
		resources.append({
			"path": path,
			"resource_type": "PackedScene" if path.get_extension().to_lower() == "tscn" else "Resource",
			"uid": ResourceUID.path_to_uid(path),
			"sha256": FileAccess.get_sha256(path),
			"loaded": bool(loaded.get(path, false)),
			"dependencies": capture_dependencies(path),
		})
	var main_raw := str(ProjectSettings.get_setting("application/run/main_scene", ""))
	return {
		"project_sha256": FileAccess.get_sha256("res://project.godot"),
		"main_scene": {"raw": main_raw, "resolved": resolve_reference(main_raw)},
		"autoloads": capture_autoloads(),
		"resources": resources,
	}

func capture_autoloads() -> Array[Dictionary]:
	var result: Array[Dictionary] = []
	for property in ProjectSettings.get_property_list():
		var setting_name := str(property.get("name", ""))
		if setting_name.begins_with("autoload/"):
			var raw := str(ProjectSettings.get_setting(setting_name, ""))
			result.append({
				"name": setting_name.trim_prefix("autoload/"),
				"raw": raw,
				"resolved": resolve_reference(raw),
			})
	return result

func capture_dependencies(path: String) -> Array[Dictionary]:
	var result: Array[Dictionary] = []
	for raw_value in ResourceLoader.get_dependencies(path):
		var raw := str(raw_value)
		var parts := raw.split("::", false)
		var uid := ""
		var type_name := ""
		var fallback := raw
		if parts.size() == 3:
			uid = parts[0]
			type_name = parts[1]
			fallback = parts[2]
		var resolved := resolve_reference(uid if uid.begins_with("uid://") else fallback)
		if resolved.is_empty():
			resolved = resolve_reference(fallback)
		var exists := not resolved.is_empty() and (ResourceLoader.exists(resolved) or FileAccess.file_exists(resolved))
		result.append({
			"raw": raw,
			"uid": uid,
			"type_name": type_name,
			"fallback_path": fallback,
			"resolved": resolved,
			"exists": exists,
			"sha256": FileAccess.get_sha256(resolved) if exists and FileAccess.file_exists(resolved) else "",
		})
	return result

func resolve_reference(value: String) -> String:
	var cleaned := value.trim_prefix("*")
	if cleaned.is_empty():
		return ""
	return ResourceUID.ensure_path(cleaned)
"#;

#[cfg(test)]
mod tests {
    use super::{extract_diagnostics, sanitize_diagnostic};
    use crate::provider::{CapturedOutput, ProcessResult};
    use std::{path::Path, process::ExitStatus, time::Duration};

    #[cfg(unix)]
    fn success_status() -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }

    #[cfg(windows)]
    fn success_status() -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }

    fn output(bytes: &[u8]) -> CapturedOutput {
        CapturedOutput {
            bytes: bytes.to_vec(),
            total_bytes: bytes.len(),
            sha256: "hash".to_owned(),
            truncated: false,
        }
    }

    #[test]
    fn extracts_only_errors_and_removes_machine_paths() {
        let process = ProcessResult {
            status: success_status(),
            timed_out: false,
            duration: Duration::ZERO,
            stdout: output(b"normal\nERROR: C:/repo/scenes/main.tscn failed\n"),
            stderr: output(b"SCRIPT ERROR: C:/probe/probe.gd:2\n"),
        };
        let diagnostics =
            extract_diagnostics([&process], Path::new("C:/repo"), Path::new("C:/probe"));
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics.iter().all(|item| !item.contains("C:/repo")));
        assert!(diagnostics.iter().all(|item| !item.contains("C:/probe")));
        assert_eq!(
            sanitize_diagnostic(
                "ERROR: C:/repo/a",
                Path::new("C:/repo"),
                Path::new("C:/probe")
            ),
            "ERROR: <project>/a"
        );
    }
}
