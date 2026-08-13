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

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
    let project = root.join("repo");
    let unknown = root.join("unknown");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&unknown).unwrap();
    fs::create_dir_all(&codex_home).unwrap();
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
    assert_eq!(doctor["status"], "ready");
    assert_eq!(doctor["providers"], "pass");
    assert_eq!(
        doctor["codex_trust_state"],
        "not_programmatically_verifiable"
    );

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
    assert_eq!(degraded["codex_hooks"], "fail");

    fs::remove_dir_all(root).unwrap();
}
