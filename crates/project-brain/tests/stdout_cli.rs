use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_project-brain"))
}

fn unique_project_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time must follow Unix epoch")
        .as_nanos();
    env::temp_dir().join(format!(
        "project-brain-broken-pipe-{}-{nonce}",
        std::process::id()
    ))
}

#[test]
fn closed_stdout_pipe_exits_successfully_without_panic() {
    let root = unique_project_root();
    fs::create_dir_all(&root).expect("temporary project root should be created");

    let mut child = Command::new(binary())
        .args([
            "--project-root",
            root.to_str().expect("UTF-8 temp path"),
            "init",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("project-brain should start");

    drop(child.stdout.take());
    let output = child.wait_with_output().expect("child should finish");

    assert!(
        output.status.success(),
        "closed stdout must be treated as a normal downstream termination: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("panicked at"),
        "broken stdout must not emit a panic"
    );

    fs::remove_dir_all(root).expect("temporary project root should be removed");
}
