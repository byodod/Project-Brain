use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

fn temp_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("valid clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "project-brain-cli-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create temp root");
    root
}

fn run(executable: &Path, arguments: &[&str], cwd: &Path, stdin: Option<&str>) -> Output {
    let mut child = Command::new(executable)
        .args(arguments)
        .current_dir(cwd)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn project-brain");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .expect("stdin pipe")
            .write_all(input.as_bytes())
            .expect("write stdin");
    }
    child.wait_with_output().expect("wait for project-brain")
}

fn run_installed_handler(handler: &Value, cwd: &Path, stdin: &str) -> Output {
    let command = handler["command"].as_str().expect("handler command");
    let arguments: Vec<&str> = handler["args"]
        .as_array()
        .expect("handler args")
        .iter()
        .map(|argument| argument.as_str().expect("string argument"))
        .collect();
    let mut child = Command::new(command)
        .args(arguments)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn installed exec-form handler");
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(stdin.as_bytes())
        .expect("write handler stdin");
    child.wait_with_output().expect("wait for handler")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn godot_evidence_requires_explicit_machine_executable_trust() {
    let root = temp_root("godot-trust");
    let project = root.join("repo");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("project.godot"), b"[application]\n").unwrap();
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_project-brain"));
    assert_success(&run(
        &executable,
        &["--project-root", project.to_str().unwrap(), "init"],
        &root,
        None,
    ));
    let fake_engine = root.join("fake-godot");
    fs::write(&fake_engine, b"not executable").unwrap();
    let output = run(
        &executable,
        &[
            "--project-root",
            project.to_str().unwrap(),
            "evidence",
            "godot",
            "--executable",
            fake_engine.to_str().unwrap(),
        ],
        &project,
        None,
    );

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--trust-local-executable"),
        "unexpected stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn prime_direct_adapter_blocks_without_claiming_stop_continuation() {
    let root = temp_root("prime-direct");
    let project = root.join("repo");
    fs::create_dir_all(&project).unwrap();
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_project-brain"));
    assert_success(&run(
        &executable,
        &[
            "--project-root",
            project.to_str().unwrap(),
            "init",
            "--profile",
            "rust",
        ],
        &root,
        None,
    ));

    let config_path = project.join(".project-brain/config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["rules"] = serde_json::json!([{
        "id": "PROTECT",
        "status": "active",
        "authority": "repository_rule",
        "strength": "hard",
        "effect": "block",
        "include_paths": [".project-brain/config.json"],
        "actions": ["modify"],
        "message": "protected"
    }]);
    fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let input = format!(
        "{{\"session_id\":\"prime-session\",\"cwd\":{},\"tool_name\":\"edit\",\"tool_use_id\":\"prime-tool\",\"tool_input\":{{\"path\":\".project-brain/config.json\",\"oldText\":\"old\",\"newText\":\"new\"}}}}",
        serde_json::to_string(project.to_str().unwrap()).unwrap()
    );
    let hook = run(
        &executable,
        &[
            "--project-root",
            project.to_str().unwrap(),
            "hook",
            "prime-agent",
            "pre-tool-use",
        ],
        &project,
        Some(&input),
    );
    assert_success(&hook);
    let output: Value = serde_json::from_slice(&hook.stdout).unwrap();
    assert_eq!(output["schema_version"], 1);
    assert_eq!(output["event"], "tool_about_to_run");
    assert_eq!(output["block"], true);

    let capabilities = run(
        &executable,
        &["capabilities", "prime-agent"],
        &project,
        None,
    );
    assert_success(&capabilities);
    let capabilities: Value = serde_json::from_slice(&capabilities.stdout).unwrap();
    assert_eq!(capabilities["continue_after_stop"], "unsupported");
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "单条黑盒测试按真实用户顺序验证安装到卸载的完整事务边界"
)]
fn install_bootstrap_dispatch_doctor_and_uninstall_are_end_to_end() {
    let root = temp_root("e2e");
    let install_root = root.join("machine/Project Brain");
    let codex_home = root.join("Codex Home");
    let claude_home = root.join("Claude Home");
    let project = root.join("repo");
    let unknown = root.join("unknown");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&unknown).unwrap();
    fs::create_dir_all(&codex_home).unwrap();
    fs::create_dir_all(&claude_home).unwrap();
    let user_hooks = "{\n  \"custom\": true,\n  \"hooks\": {\n    \"Stop\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"user-stop\"}]}]\n  }\n}\n";
    fs::write(codex_home.join("hooks.json"), user_hooks).unwrap();

    let source = PathBuf::from(env!("CARGO_BIN_EXE_project-brain"));
    let install = run(
        &source,
        &["--install-root", install_root.to_str().unwrap(), "install"],
        &root,
        None,
    );
    assert_success(&install);
    let launcher = install_root.join("bin").join(
        source
            .file_name()
            .expect("source executable has a file name"),
    );
    assert!(launcher.is_file());
    let rollback_version = "0.0.9-test";
    let rollback_payload = install_root
        .join("versions")
        .join(rollback_version)
        .join(source.file_name().unwrap());
    fs::create_dir_all(rollback_payload.parent().unwrap()).unwrap();
    fs::copy(&source, &rollback_payload).unwrap();
    fs::write(
        install_root.join("state/install.json"),
        format!(
            "{{\"schema_version\":1,\"current\":{},\"previous\":{}}}\n",
            serde_json::to_string(env!("CARGO_PKG_VERSION")).unwrap(),
            serde_json::to_string(rollback_version).unwrap()
        ),
    )
    .unwrap();
    let rollback = run(
        &launcher,
        &["--install-root", install_root.to_str().unwrap(), "rollback"],
        &root,
        None,
    );
    assert_success(&rollback);
    let rollback: Value = serde_json::from_slice(&rollback.stdout).unwrap();
    assert_eq!(rollback["current_version"], rollback_version);
    assert_eq!(rollback["stable_launcher_unchanged"], true);

    let init = run(
        &launcher,
        &[
            "--project-root",
            project.to_str().unwrap(),
            "init",
            "--profile",
            "dotnet",
            "--profile",
            "python",
        ],
        &root,
        None,
    );
    assert_success(&init);
    let config: Value =
        serde_json::from_slice(&fs::read(project.join(".project-brain/config.json")).unwrap())
            .unwrap();
    assert_eq!(config["language_profiles"].as_array().unwrap().len(), 3);
    assert!(
        !serde_json::to_string(&config)
            .unwrap()
            .contains(install_root.to_str().unwrap())
    );

    let bootstrap_args = [
        "--install-root",
        install_root.to_str().unwrap(),
        "--codex-home",
        codex_home.to_str().unwrap(),
        "--project-root",
        project.to_str().unwrap(),
        "bootstrap",
        "--codex",
    ];
    fs::write(codex_home.join("hooks.json"), "{ malformed").unwrap();
    let failed_bootstrap = run(&launcher, &bootstrap_args, &root, None);
    assert!(!failed_bootstrap.status.success());
    assert_eq!(
        fs::read_to_string(codex_home.join("hooks.json")).unwrap(),
        "{ malformed"
    );
    let registry_after_failure: Value =
        serde_json::from_slice(&fs::read(install_root.join("state/projects.json")).unwrap())
            .unwrap();
    assert_eq!(registry_after_failure["projects"], serde_json::json!([]));
    fs::write(codex_home.join("hooks.json"), user_hooks).unwrap();

    let fake_dir = install_root.join("provider-fixtures");
    fs::create_dir_all(&fake_dir).unwrap();
    for (profile, producer) in [
        ("dotnet-main", "scip-dotnet"),
        ("python-main", "scip-python"),
    ] {
        let fake = fake_dir.join(format!("{producer}{}", std::env::consts::EXE_SUFFIX));
        fs::copy(&source, &fake).unwrap();
        let binding = run(
            &launcher,
            &[
                "--install-root",
                install_root.to_str().unwrap(),
                "--project-root",
                project.to_str().unwrap(),
                "provider",
                "bind",
                "--profile",
                profile,
                "--executable",
                fake.to_str().unwrap(),
                "--trust-local-executable",
            ],
            &root,
            None,
        );
        assert_success(&binding);
    }

    assert_success(&run(&launcher, &bootstrap_args, &root, None));
    let second_bootstrap = run(&launcher, &bootstrap_args, &root, None);
    assert_success(&second_bootstrap);
    let second: Value = serde_json::from_slice(&second_bootstrap.stdout).unwrap();
    assert_eq!(second["registered"], false);

    let hooks: Value =
        serde_json::from_slice(&fs::read(codex_home.join("hooks.json")).unwrap()).unwrap();
    assert_eq!(hooks["custom"], true);
    assert_eq!(hooks["hooks"]["Stop"].as_array().unwrap().len(), 2);
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Stop",
    ] {
        assert!(hooks["hooks"][event].is_array());
    }

    let unknown_input = format!(
        "{{\"session_id\":\"s\",\"cwd\":{},\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"apply_patch\",\"tool_use_id\":\"u\",\"tool_input\":{{}}}}",
        serde_json::to_string(unknown.to_str().unwrap()).unwrap()
    );
    let unknown_dispatch = run(
        &launcher,
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "dispatch",
            "codex",
            "pre-tool-use",
        ],
        &unknown,
        Some(&unknown_input),
    );
    assert_success(&unknown_dispatch);
    assert!(unknown_dispatch.stdout.is_empty());

    let registered_input = format!(
        "{{\"session_id\":\"s\",\"cwd\":{},\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"apply_patch\",\"tool_use_id\":\"u\",\"tool_input\":{{\"command\":\"*** Begin Patch\\n*** Delete File: .project-brain/config.json\\n*** End Patch\"}}}}",
        serde_json::to_string(project.to_str().unwrap()).unwrap()
    );
    let registered_dispatch = run(
        &launcher,
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "dispatch",
            "codex",
            "pre-tool-use",
        ],
        &project,
        Some(&registered_input),
    );
    assert_success(&registered_dispatch);
    let decision: Value = serde_json::from_slice(&registered_dispatch.stdout).unwrap();
    assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");

    let doctor = run(
        &launcher,
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "--codex-home",
            codex_home.to_str().unwrap(),
            "--project-root",
            project.to_str().unwrap(),
            "doctor",
        ],
        &project,
        None,
    );
    assert_success(&doctor);
    let doctor: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(doctor["schema_version"], 2);
    assert_eq!(doctor["status"], "ready");
    assert_eq!(doctor["providers"], "pass");
    assert_eq!(
        doctor["adapter_trust_state"],
        "not_programmatically_verifiable"
    );
    assert_eq!(doctor["adapter"], "codex");
    assert_eq!(doctor["adapter_hooks"], "pass");

    assert_success(&run(
        &launcher,
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "--claude-home",
            claude_home.to_str().unwrap(),
            "install-hooks",
            "claude-code",
        ],
        &root,
        None,
    ));
    let claude_settings: Value =
        serde_json::from_slice(&fs::read(claude_home.join("settings.json")).unwrap()).unwrap();
    let claude_handler = claude_settings["hooks"]["PreToolUse"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["hooks"].as_array().unwrap())
        .find(|handler| {
            handler["args"]
                .as_array()
                .is_some_and(|args| args.get(1) == Some(&serde_json::json!("claude-code")))
        })
        .unwrap();
    let claude_input = format!(
        "{{\"session_id\":\"claude-session\",\"cwd\":{},\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"Bash\",\"tool_use_id\":\"claude-tool\",\"tool_input\":{{\"command\":\"rm .project-brain/config.json\"}}}}",
        serde_json::to_string(project.to_str().unwrap()).unwrap()
    );
    let claude_process = run_installed_handler(claude_handler, &project, &claude_input);
    assert_success(&claude_process);
    let claude_decision: Value =
        serde_json::from_slice(&claude_process.stdout).unwrap_or_else(|error| {
            panic!(
                "installed Claude handler returned invalid JSON: {error}\nstdout={}\nstderr={}",
                String::from_utf8_lossy(&claude_process.stdout),
                String::from_utf8_lossy(&claude_process.stderr)
            )
        });
    assert_eq!(
        claude_decision["hookSpecificOutput"]["permissionDecision"], "deny",
        "unexpected Claude decision: {claude_decision}"
    );
    let claude_doctor = run(
        &launcher,
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "--claude-home",
            claude_home.to_str().unwrap(),
            "--project-root",
            project.to_str().unwrap(),
            "doctor",
            "claude-code",
        ],
        &project,
        None,
    );
    assert_success(&claude_doctor);
    let claude_doctor: Value = serde_json::from_slice(&claude_doctor.stdout).unwrap();
    assert_eq!(claude_doctor["schema_version"], 2);
    assert_eq!(claude_doctor["adapter"], "claude_code");
    assert_eq!(claude_doctor["adapter_hooks"], "pass");

    let hooks_before_drift = fs::read(codex_home.join("hooks.json")).unwrap();
    let mut drifted: Value = serde_json::from_slice(&hooks_before_drift).unwrap();
    drifted["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"] = serde_json::json!(99);
    let drifted_bytes = serde_json::to_vec_pretty(&drifted).unwrap();
    fs::write(codex_home.join("hooks.json"), &drifted_bytes).unwrap();
    let drift_install = run(
        &launcher,
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "--codex-home",
            codex_home.to_str().unwrap(),
            "install-hooks",
            "codex",
        ],
        &root,
        None,
    );
    assert!(!drift_install.status.success());
    assert_eq!(
        fs::read(codex_home.join("hooks.json")).unwrap(),
        drifted_bytes
    );
    fs::write(codex_home.join("hooks.json"), hooks_before_drift).unwrap();

    let uninstall = run(
        &launcher,
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "--codex-home",
            codex_home.to_str().unwrap(),
            "uninstall-hooks",
            "codex",
        ],
        &root,
        None,
    );
    assert_success(&uninstall);
    let hooks_after: Value =
        serde_json::from_slice(&fs::read(codex_home.join("hooks.json")).unwrap()).unwrap();
    assert_eq!(hooks_after["custom"], true);
    assert_eq!(hooks_after["hooks"]["Stop"].as_array().unwrap().len(), 1);
    assert_eq!(
        hooks_after["hooks"]["Stop"][0]["hooks"][0]["command"],
        "user-stop"
    );

    let degraded_doctor = run(
        &launcher,
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "--codex-home",
            codex_home.to_str().unwrap(),
            "--project-root",
            project.to_str().unwrap(),
            "doctor",
        ],
        &project,
        None,
    );
    assert!(!degraded_doctor.status.success());
    let degraded: Value = serde_json::from_slice(&degraded_doctor.stdout).unwrap();
    assert_eq!(degraded["status"], "degraded");
    assert_eq!(degraded["adapter"], "codex");
    assert_eq!(degraded["adapter_hooks"], "fail");

    fs::remove_dir_all(root).unwrap();
}

fn assert_claude_hooks_installed(settings: &Value) {
    assert_eq!(settings["language"], "chinese");
    assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 2);
    for (event, event_arg) in [
        ("SessionStart", "session-start"),
        ("UserPromptSubmit", "user-prompt-submit"),
        ("PreToolUse", "pre-tool-use"),
        ("PostToolUse", "post-tool-use"),
        ("Stop", "stop"),
    ] {
        let managed = settings["hooks"][event]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|group| group["hooks"].as_array().unwrap())
            .find(|handler| {
                handler["args"] == serde_json::json!(["dispatch", "claude-code", event_arg])
            })
            .unwrap();
        assert_eq!(managed["type"], "command");
        assert_eq!(managed["timeout"], 10);
        assert_eq!(
            managed["statusMessage"],
            "Project Brain deterministic governance"
        );
        let command = Path::new(managed["command"].as_str().unwrap());
        assert!(command.is_absolute());
        assert!(command.is_file());
        assert!(managed.get("commandWindows").is_none());
        assert!(managed.get("shell").is_none());
    }
}

fn drift_claude_pre_tool_timeout(settings: &mut Value) {
    let managed_group = settings["hooks"]["PreToolUse"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|group| {
            group["hooks"].as_array().unwrap().iter().any(|handler| {
                handler["args"] == serde_json::json!(["dispatch", "claude-code", "pre-tool-use"])
            })
        })
        .unwrap();
    managed_group["hooks"][0]["timeout"] = serde_json::json!(99);
}

fn assert_only_user_claude_hook_remains(settings: &Value) {
    assert_eq!(settings["language"], "chinese");
    assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 1);
    assert_eq!(
        settings["hooks"]["Stop"][0]["hooks"][0]["command"],
        "user-stop"
    );
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
    ] {
        assert!(
            settings["hooks"][event]
                .as_array()
                .is_none_or(Vec::is_empty)
        );
    }
}

#[test]
fn claude_hook_install_is_atomic_idempotent_and_preserves_user_settings() {
    let root = temp_root("claude-hooks");
    let install_root = root.join("machine/Project Brain");
    let claude_home = root.join("Claude Home");
    fs::create_dir_all(&claude_home).unwrap();
    let user_settings = serde_json::json!({
        "language": "chinese",
        "hooks": {
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": "user-stop",
                    "timeout": 3
                }]
            }]
        }
    });
    fs::write(
        claude_home.join("settings.json"),
        serde_json::to_vec_pretty(&user_settings).unwrap(),
    )
    .unwrap();

    let source = PathBuf::from(env!("CARGO_BIN_EXE_project-brain"));
    assert_success(&run(
        &source,
        &["--install-root", install_root.to_str().unwrap(), "install"],
        &root,
        None,
    ));
    let launcher = install_root.join("bin").join(source.file_name().unwrap());
    let install_args = [
        "--install-root",
        install_root.to_str().unwrap(),
        "--claude-home",
        claude_home.to_str().unwrap(),
        "install-hooks",
        "claude-code",
    ];
    let first = run(&launcher, &install_args, &root, None);
    assert_success(&first);
    let first_report: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_report["changed"], true);
    assert_eq!(first_report["managed_handler_count"], 5);

    let settings_path = claude_home.join("settings.json");
    let installed_bytes = fs::read(&settings_path).unwrap();
    let installed: Value = serde_json::from_slice(&installed_bytes).unwrap();
    assert_claude_hooks_installed(&installed);

    let second = run(&launcher, &install_args, &root, None);
    assert_success(&second);
    let second_report: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second_report["changed"], false);
    assert_eq!(fs::read(&settings_path).unwrap(), installed_bytes);

    let mut drifted = installed.clone();
    drift_claude_pre_tool_timeout(&mut drifted);
    let drifted_bytes = serde_json::to_vec_pretty(&drifted).unwrap();
    fs::write(&settings_path, &drifted_bytes).unwrap();
    let drift_install = run(&launcher, &install_args, &root, None);
    assert!(!drift_install.status.success());
    assert_eq!(fs::read(&settings_path).unwrap(), drifted_bytes);

    fs::write(&settings_path, installed_bytes).unwrap();
    let uninstall = run(
        &launcher,
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "--claude-home",
            claude_home.to_str().unwrap(),
            "uninstall-hooks",
            "claude-code",
        ],
        &root,
        None,
    );
    assert_success(&uninstall);
    let after: Value = serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
    assert_only_user_claude_hook_remains(&after);

    fs::remove_dir_all(root).unwrap();
}
