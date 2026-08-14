use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use brain_evidence::{DependencyCoverage, InputDependencyContractV1, InputRole, InputSelectorV1};
use serde_json::Value;

fn temp_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "project-brain-evidence-provider-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn project_brain() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_project-brain"))
}

fn reference_provider() -> Option<PathBuf> {
    let executable_path = project_brain();
    let debug = executable_path.parent()?;
    let executable = debug.join("examples").join(if cfg!(windows) {
        "reference_provider.exe"
    } else {
        "reference_provider"
    });
    executable.is_file().then_some(executable)
}

fn run(executable: &Path, arguments: &[&str], cwd: &Path) -> Output {
    Command::new(executable)
        .args(arguments)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git(project: &Path, arguments: &[&str]) {
    success(&run(Path::new("git"), arguments, project));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "端到端 fixture 连续证明绑定、staging、持久化和输入失效"
)]
fn generic_provider_is_bound_staged_persisted_and_invalidated_by_declared_input() {
    let Some(reference) = reference_provider() else {
        // `cargo test --workspace --all-targets`（CI 合同）会构建 reference example。
        // 单独运行此 integration target 时不递归启动 Cargo，以避免构建锁死锁。
        return;
    };
    let root = temp_root();
    let install_root = root.join("install");
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    success(&run(
        &project_brain(),
        &["--install-root", install_root.to_str().unwrap(), "install"],
        &root,
    ));
    success(&run(
        &project_brain(),
        &["--project-root", project.to_str().unwrap(), "init"],
        &root,
    ));
    fs::write(project.join("input.txt"), "first\n").unwrap();
    git(&project, &["init", "-b", "main"]);
    git(&project, &["config", "user.name", "Project Brain Test"]);
    git(
        &project,
        &["config", "user.email", "project-brain@example.invalid"],
    );
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "fixture"]);
    success(&run(
        &project_brain(),
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "--project-root",
            project.to_str().unwrap(),
            "bootstrap",
        ],
        &root,
    ));

    let config: Value =
        serde_json::from_slice(&fs::read(project.join(".project-brain/config.json")).unwrap())
            .unwrap();
    let project_key = config["project_key"].as_str().unwrap();
    let contract = InputDependencyContractV1::new(
        project_key,
        "reference-main",
        "reference-provider",
        1,
        "sha256_reference_profile",
        vec![InputSelectorV1::ExactPath {
            path: "input.txt".to_owned(),
            role: InputRole::Source,
            presence_sensitive: true,
        }],
        DependencyCoverage::Complete,
    )
    .unwrap();
    let contract_path = project.join("provider-contract.json");
    fs::write(
        &contract_path,
        serde_json::to_vec_pretty(&contract).unwrap(),
    )
    .unwrap();
    git(&project, &["add", "provider-contract.json"]);
    git(&project, &["commit", "-m", "provider contract"]);

    let provider_copy = root.join(if cfg!(windows) {
        "reference-provider.exe"
    } else {
        "reference-provider"
    });
    fs::copy(reference, &provider_copy).unwrap();
    let common = [
        "--install-root",
        install_root.to_str().unwrap(),
        "--project-root",
        project.to_str().unwrap(),
    ];
    let mut bind = common.to_vec();
    bind.extend([
        "evidence",
        "provider",
        "bind",
        "--profile",
        "reference-main",
        "--executable",
        provider_copy.to_str().unwrap(),
        "--authority-ceiling",
        "heuristic",
        "--trust-local-executable",
    ]);
    success(&run(&project_brain(), &bind, &root));

    let mut execute = common.to_vec();
    execute.extend([
        "evidence",
        "provider",
        "run",
        "--profile",
        "reference-main",
        "--plane",
        "build",
        "--contract",
        contract_path.to_str().unwrap(),
    ]);
    let output = run(&project_brain(), &execute, &root);
    success(&output);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report.pointer("/run/provider_id").unwrap(),
        "reference-provider"
    );
    assert_eq!(report.pointer("/run/evidence/plane").unwrap(), "build");
    assert_eq!(report.pointer("/persistence/freshness").unwrap(), "fresh");

    fs::write(project.join("input.txt"), "second\n").unwrap();
    let mut status = common.to_vec();
    status.extend(["evidence", "status"]);
    let output = run(&project_brain(), &status, &root);
    success(&output);
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        status.pointer("/heads/0/effective_freshness").unwrap(),
        "stale"
    );

    fs::remove_dir_all(root).unwrap();
}
