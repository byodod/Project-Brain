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

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_project-brain"))
}

fn run(
    executable: &Path,
    arguments: &[&str],
    cwd: &Path,
    stdin: Option<&str>,
    environment: &[(&str, &Path)],
) -> Output {
    let mut command = Command::new(executable);
    command.args(arguments).current_dir(cwd);
    for (name, value) in environment {
        command.env(name, value);
    }
    let mut child = command
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
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
    }
    child.wait_with_output().expect("wait for project-brain")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn install_and_init(root: &Path) -> (PathBuf, PathBuf) {
    let executable = binary();
    let install_root = root.join("install root");
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    let install = run(
        &executable,
        &["--install-root", install_root.to_str().unwrap(), "install"],
        root,
        None,
        &[],
    );
    assert_success(&install);
    let initialize = run(
        &executable,
        &["--project-root", project.to_str().unwrap(), "init"],
        root,
        None,
        &[],
    );
    assert_success(&initialize);
    let bootstrap = run(
        &executable,
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "--project-root",
            project.to_str().unwrap(),
            "bootstrap",
        ],
        root,
        None,
        &[],
    );
    assert_success(&bootstrap);
    (install_root, project)
}

fn add_protected_rule(project: &Path) {
    let config_path = project.join(".project-brain/config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["rules"] = serde_json::json!([{
        "id": "PB-TEST-001",
        "status": "active",
        "effect": "block",
        "strength": "hard",
        "authority": "repository_rule",
        "include_paths": [".project-brain/config.json"],
        "exclude_paths": [],
        "actions": ["delete"],
        "operations": [],
        "operation_contains": [],
        "symbol_scopes": [],
        "message": "禁止删除控制面配置",
        "rationale": "黑盒验证真实 hard rule"
    }]);
    fs::write(
        config_path,
        serde_json::to_vec_pretty(&config).expect("serialize config"),
    )
    .unwrap();
}

fn blocking_input(project: &Path, adapter: &str) -> String {
    serde_json::json!({
        "session_id": format!("{adapter}-session"),
        "cwd": project,
        "hook_event_name": "PreToolUse",
        "tool_name": "shell_command",
        "tool_use_id": format!("{adapter}-tool"),
        "tool_input": {"command": "Remove-Item .project-brain/config.json"}
    })
    .to_string()
}

#[test]
fn capability_matrix_is_explicit_for_all_supported_agents() {
    let root = temp_root("capabilities");
    for agent in ["codex", "pi", "opencode", "dsh"] {
        let output = run(&binary(), &["capabilities", agent], &root, None, &[]);
        assert_success(&output);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["deny_tool"], "supported", "adapter={agent}");
        assert_eq!(value["post_feedback"], "supported", "adapter={agent}");
    }
    let opencode = run(&binary(), &["capabilities", "opencode"], &root, None, &[]);
    let opencode: Value = serde_json::from_slice(&opencode.stdout).unwrap();
    assert_eq!(opencode["continue_after_stop"], "unsupported");
    let pi = run(&binary(), &["capabilities", "pi"], &root, None, &[]);
    let pi: Value = serde_json::from_slice(&pi.stdout).unwrap();
    assert_eq!(pi["continue_after_stop"], "emulated");
}

#[test]
fn normalized_dispatch_blocks_protected_deletion_for_all_agents() {
    let root = temp_root("dispatch");
    let (install_root, project) = install_and_init(&root);
    add_protected_rule(&project);
    for agent in ["codex", "pi", "opencode", "dsh"] {
        let input = blocking_input(&project, agent);
        let output = run(
            &binary(),
            &[
                "--install-root",
                install_root.to_str().unwrap(),
                "hook",
                agent,
                "pre-tool-use",
            ],
            &project,
            Some(&input),
            &[],
        );
        assert_success(&output);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        if agent == "codex" {
            assert_eq!(
                value.pointer("/hookSpecificOutput/permissionDecision"),
                Some(&Value::String("deny".to_owned()))
            );
        } else {
            assert_eq!(value["block"], true, "adapter={agent}, output={value}");
        }
    }
}

#[test]
fn pi_extension_install_is_idempotent_drift_safe_and_removable() {
    let root = temp_root("pi-extension");
    let (install_root, project) = install_and_init(&root);
    let pi_home = root.join("pi home");
    fs::create_dir_all(pi_home.join("extensions")).unwrap();
    fs::write(
        pi_home.join("extensions/user-extension.ts"),
        "export default function userExtension() {}\n",
    )
    .unwrap();
    let common = [
        "--install-root",
        install_root.to_str().unwrap(),
        "--pi-home",
        pi_home.to_str().unwrap(),
    ];
    let mut install_args = common.to_vec();
    install_args.extend(["install-hooks", "pi"]);
    let first = run(&binary(), &install_args, &project, None, &[]);
    assert_success(&first);
    let second = run(&binary(), &install_args, &project, None, &[]);
    assert_success(&second);

    let extension = pi_home.join("extensions/project-brain/index.ts");
    let source = fs::read_to_string(&extension).unwrap();
    for event in [
        "session_start",
        "input",
        "before_agent_start",
        "tool_call",
        "tool_result",
        "agent_end",
    ] {
        assert!(source.contains(event), "missing PI event {event}");
    }
    assert!(source.contains("triggerTurn: true"));
    assert!(pi_home.join("extensions/user-extension.ts").is_file());

    fs::write(&extension, "// drift\n").unwrap();
    let mut uninstall_args = common.to_vec();
    uninstall_args.extend(["uninstall-hooks", "pi"]);
    let refused = run(&binary(), &uninstall_args, &project, None, &[]);
    assert!(!refused.status.success());
    assert!(extension.is_file());
    uninstall_args.push("--force");
    let forced = run(&binary(), &uninstall_args, &project, None, &[]);
    assert_success(&forced);
    assert!(!pi_home.join("extensions/project-brain").exists());
    assert!(pi_home.join("extensions/user-extension.ts").is_file());
}

#[test]
fn opencode_plugin_install_is_idempotent_drift_safe_and_removable() {
    let root = temp_root("opencode-plugin");
    let (install_root, project) = install_and_init(&root);
    let opencode_home = root.join("opencode home");
    fs::create_dir_all(opencode_home.join("plugins")).unwrap();
    fs::write(
        opencode_home.join("plugins/user-plugin.js"),
        "export const UserPlugin = async () => ({})\n",
    )
    .unwrap();
    let common = [
        "--install-root",
        install_root.to_str().unwrap(),
        "--opencode-home",
        opencode_home.to_str().unwrap(),
    ];
    let mut install_args = common.to_vec();
    install_args.extend(["install-hooks", "opencode"]);
    let first = run(&binary(), &install_args, &project, None, &[]);
    assert_success(&first);
    let second = run(&binary(), &install_args, &project, None, &[]);
    assert_success(&second);

    let plugin = opencode_home.join("plugins/project-brain.js");
    let source = fs::read_to_string(&plugin).unwrap();
    for event in [
        "chat.message",
        "tool.execute.before",
        "tool.execute.after",
        "session.created",
        "session.idle",
    ] {
        assert!(source.contains(event), "missing opencode event {event}");
    }
    assert!(source.contains("throw new Error"));
    assert!(opencode_home.join("plugins/user-plugin.js").is_file());

    let syntax = Command::new("node").arg("--check").arg(&plugin).output();
    if let Ok(syntax) = syntax {
        assert_success(&syntax);
    }

    fs::write(&plugin, "// drift\n").unwrap();
    let mut uninstall_args = common.to_vec();
    uninstall_args.extend(["uninstall-hooks", "opencode"]);
    let refused = run(&binary(), &uninstall_args, &project, None, &[]);
    assert!(!refused.status.success());
    uninstall_args.push("--force");
    let forced = run(&binary(), &uninstall_args, &project, None, &[]);
    assert_success(&forced);
    assert!(!plugin.exists());
    assert!(opencode_home.join("plugins/user-plugin.js").is_file());
}

fn compile_fake_dsh(root: &Path) -> Option<PathBuf> {
    let source = root.join("fake-dsh.rs");
    fs::write(
        &source,
        r##"
use std::{env, fs, path::PathBuf};

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(args.first().map(String::as_str), Some("plugin"));
    assert_eq!(args.get(1).map(String::as_str), Some("--profile"));
    let profile = args.get(2).expect("profile");
    let operation = args.get(3).expect("operation");
    let home = PathBuf::from(env::var_os("DSH_HOME").expect("DSH_HOME"));
    let profile_root = home.join("profiles").join(profile);
    let package_root = profile_root.join("node_modules/@project-brain/dsh-plugin");
    fs::create_dir_all(&profile_root).unwrap();
    match operation.as_str() {
        "add" => {
            let source = PathBuf::from(args.get(4).expect("source").strip_prefix("file:").unwrap());
            fs::create_dir_all(package_root.join("lib")).unwrap();
            fs::copy(source.join("lib/index.js"), package_root.join("lib/index.js")).unwrap();
            fs::copy(source.join("package.json"), package_root.join("package.json")).unwrap();
            fs::write(
                profile_root.join("package.json"),
                r#"{"dependencies":{"@project-brain/dsh-plugin":"file:managed"},"dsh":{"profile":{"bundles":["@project-brain/dsh-plugin"]}}}"#,
            ).unwrap();
        }
        "remove" => {
            if package_root.exists() {
                fs::remove_dir_all(&package_root).unwrap();
            }
            fs::write(
                profile_root.join("package.json"),
                r#"{"dependencies":{},"dsh":{"profile":{"bundles":[]}}}"#,
            ).unwrap();
        }
        other => panic!("unexpected operation: {other}"),
    }
}
"##,
    )
    .unwrap();
    let executable = root.join(if cfg!(windows) {
        "fake-dsh.exe"
    } else {
        "fake-dsh"
    });
    let output = Command::new("rustc")
        .args(["--edition=2024", source.to_str().unwrap(), "-o"])
        .arg(&executable)
        .output()
        .ok()?;
    assert_success(&output);
    Some(executable)
}

fn node_runtime() -> Option<PathBuf> {
    let executable = PathBuf::from("node");
    let output = Command::new(&executable).arg("--version").output().ok()?;
    output.status.success().then_some(executable)
}

// 内嵌脚本是跨平台黑盒 fixture；保留在单一函数中可确保写入与执行的是同一份审计文本。
#[allow(clippy::too_many_lines)]
fn run_adapter_runtime_harness(
    node: &Path,
    root: &Path,
    adapter: &str,
    plugin: &Path,
    project: &Path,
) {
    let harness = root.join("adapter-runtime-harness.mjs");
    fs::write(
        &harness,
        r#"import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

const [adapter, pluginPath, project] = process.argv.slice(2);
const plugin = await import(pathToFileURL(pluginPath).href);
const protectedCommand = "Remove-Item .project-brain/config.json";

if (adapter === "pi") {
  const handlers = new Map();
  const sent = [];
  const pi = {
    on(name, handler) { handlers.set(name, handler); },
    sendMessage(message, options) { sent.push({ message, options }); },
  };
  plugin.default(pi);
  for (const name of ["session_start", "input", "before_agent_start", "tool_call", "tool_result", "agent_end"]) {
    assert.equal(typeof handlers.get(name), "function", `missing Pi handler ${name}`);
  }
  const ctx = {
    cwd: project,
    sessionManager: { getSessionFile: () => "pi-runtime-session" },
  };
  await handlers.get("session_start")({ reason: "new" }, ctx);
  assert.deepEqual(await handlers.get("input")({ source: "interactive", text: "inspect project" }, ctx), { action: "continue" });
  handlers.get("before_agent_start")();
  const denied = await handlers.get("tool_call")({
    toolName: "bash",
    toolCallId: "pi-runtime-tool",
    input: { command: protectedCommand },
  }, ctx);
  assert.equal(denied?.block, true);
  await handlers.get("tool_result")({
    toolName: "bash",
    toolCallId: "pi-runtime-tool-result",
    input: { command: "git status --short" },
    isError: false,
    content: [],
  }, ctx);
  await handlers.get("agent_end")({}, ctx);
  await handlers.get("agent_end")({}, ctx);
  assert.equal(sent.filter((item) => item.options?.triggerTurn === true).length, 1);
} else if (adapter === "opencode") {
  const logs = [];
  const hooks = await plugin.ProjectBrain({
    client: { app: { log: async (entry) => { logs.push(entry); } } },
    directory: project,
    worktree: project,
  });
  const message = { parts: [{ type: "text", text: "inspect project" }] };
  await hooks["chat.message"]({ sessionID: "opencode-runtime-session", messageID: "message-1" }, message);
  let denied = false;
  try {
    await hooks["tool.execute.before"](
      { sessionID: "opencode-runtime-session", tool: "shell_command", callID: "opencode-runtime-tool" },
      { args: { command: protectedCommand } },
    );
  } catch (error) {
    denied = String(error).includes("禁止删除控制面配置");
  }
  assert.equal(denied, true);
  const after = { title: "status", output: "ok", metadata: {} };
  await hooks["tool.execute.after"]({
    sessionID: "opencode-runtime-session",
    tool: "shell_command",
    callID: "opencode-runtime-tool-result",
    args: { command: "git status --short" },
  }, after);
  await hooks.event({ event: { type: "session.created", properties: { info: { id: "opencode-runtime-session" } } } });
  await hooks.event({ event: { type: "session.idle", properties: { sessionID: "opencode-runtime-session" } } });
} else if (adapter === "dsh") {
  const handlers = new Map();
  plugin.apply({ on(name, handler) { handlers.set(name, handler); } });
  for (const name of ["agent/session-start", "agent/pre-step", "tools/pre-execute", "tools/post-execute", "agent/turn-stopping"]) {
    assert.equal(typeof handlers.get(name), "function", `missing dsh handler ${name}`);
  }
  const steered = [];
  const agent = {
    id: "dsh-runtime-session",
    session: { header: { cwd: project } },
    steer(message) { steered.push(message); },
  };
  handlers.get("agent/session-start")({ agent, source: "startup" });
  const step = await handlers.get("agent/pre-step")({
    agent,
    turn: 1,
    step: 1,
    messages: [{ source: { kind: "user" }, content: [{ type: "text", text: "inspect project" }] }],
  }, async () => ({ kind: "enter", messages: [] }));
  assert.equal(step.kind, "enter");
  const denied = await handlers.get("tools/pre-execute")({
    agent,
    name: "shell_command",
    callId: "dsh-runtime-tool",
    arguments: { command: protectedCommand },
  }, async () => ({ kind: "allow" }));
  assert.equal(denied.kind, "deny");
  const post = await handlers.get("tools/post-execute")({
    agent,
    name: "shell_command",
    callId: "dsh-runtime-tool-result",
    arguments: { command: "git status --short" },
  }, { isError: false }, async () => ({ additionalContexts: [] }));
  assert.ok(Array.isArray(post.additionalContexts));
  await handlers.get("agent/turn-stopping")({ agent, turn: 1 });
  await handlers.get("agent/turn-stopping")({ agent, turn: 1 });
  assert.equal(steered.length, 1);
} else {
  throw new Error(`unknown adapter ${adapter}`);
}
"#,
    )
    .unwrap();
    let output = Command::new(node)
        .arg(&harness)
        .args([adapter])
        .arg(plugin)
        .arg(project)
        .current_dir(project)
        .output()
        .expect("run Node adapter harness");
    assert_success(&output);
}

#[test]
fn dsh_profile_bundle_install_doctor_and_uninstall_are_verified() {
    let root = temp_root("dsh-plugin");
    let (install_root, project) = install_and_init(&root);
    let dsh_home = root.join("dsh home");
    let fake_dsh = compile_fake_dsh(&root).expect("rustc is required by Rust tests");
    let common = [
        "--install-root",
        install_root.to_str().unwrap(),
        "--dsh-home",
        dsh_home.to_str().unwrap(),
        "--dsh-profile",
        "project-brain-test",
    ];
    let mut install_args = common.to_vec();
    install_args.extend(["install-hooks", "dsh"]);
    let environment = [("PROJECT_BRAIN_DSH_EXECUTABLE", fake_dsh.as_path())];
    let first = run(&binary(), &install_args, &project, None, &environment);
    assert_success(&first);
    let second = run(&binary(), &install_args, &project, None, &environment);
    assert_success(&second);

    let plugin = dsh_home
        .join("profiles/project-brain-test/node_modules/@project-brain/dsh-plugin/lib/index.js");
    let source = fs::read_to_string(&plugin).unwrap();
    for event in [
        "agent/session-start",
        "agent/pre-step",
        "tools/pre-execute",
        "tools/post-execute",
        "agent/turn-stopping",
    ] {
        assert!(source.contains(event), "missing dsh event {event}");
    }
    assert!(source.contains("kind: \"deny\""));
    assert!(source.contains("agent.steer"));
    assert!(source.contains("invokeBrain(\"pre-step\""));
    assert!(source.contains("boundedToolResult(result)"));
    assert!(source.contains("parent_session_id"));
    assert!(source.contains("Project Brain CLI launcher"));
    let syntax = Command::new("node").arg("--check").arg(&plugin).output();
    if let Ok(syntax) = syntax {
        assert_success(&syntax);
    }

    let mut doctor_args = common.to_vec();
    doctor_args.extend(["--project-root", project.to_str().unwrap(), "doctor", "dsh"]);
    let doctor = run(&binary(), &doctor_args, &project, None, &[]);
    assert_success(&doctor);
    let doctor: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(doctor["adapter"], "dsh");
    assert_eq!(doctor["adapter_hooks"], "pass");

    let mut uninstall_args = common.to_vec();
    uninstall_args.extend(["uninstall-hooks", "dsh"]);
    let uninstall = run(&binary(), &uninstall_args, &project, None, &environment);
    assert_success(&uninstall);
    assert!(!plugin.exists());
}

#[cfg(windows)]
#[test]
fn dsh_install_discovers_npm_cmd_shim_from_path_on_windows() {
    let root = temp_root("dsh-cmd-path");
    let (install_root, project) = install_and_init(&root);
    let dsh_home = root.join("dsh home");
    let fake_dsh = compile_fake_dsh(&root).expect("rustc is required by Rust tests");
    let shim_root = root.join("npm bin");
    fs::create_dir_all(&shim_root).unwrap();
    fs::write(
        shim_root.join("dsh.cmd"),
        format!("@echo off\r\n\"{}\" %*\r\n", fake_dsh.display()),
    )
    .unwrap();
    let mut path_entries = vec![shim_root];
    if let Some(path) = std::env::var_os("PATH") {
        path_entries.extend(std::env::split_paths(&path));
    }
    let search_path = PathBuf::from(std::env::join_paths(path_entries).unwrap());

    let output = run(
        &binary(),
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "--dsh-home",
            dsh_home.to_str().unwrap(),
            "--dsh-profile",
            "cmd-shim",
            "install-hooks",
            "dsh",
        ],
        &project,
        None,
        &[("PATH", search_path.as_path())],
    );
    assert_success(&output);
    assert!(
        dsh_home
            .join("profiles/cmd-shim/node_modules/@project-brain/dsh-plugin/lib/index.js")
            .is_file()
    );
}

#[test]
fn generated_extensions_execute_real_lifecycle_and_tool_veto_roundtrips() {
    let Some(node) = node_runtime() else {
        eprintln!("node is unavailable; adapter runtime harness skipped");
        return;
    };
    let root = temp_root("adapter-runtime");
    let (install_root, project) = install_and_init(&root);
    add_protected_rule(&project);

    let pi_home = root.join("pi home");
    let pi_install = run(
        &binary(),
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "--pi-home",
            pi_home.to_str().unwrap(),
            "install-hooks",
            "pi",
        ],
        &project,
        None,
        &[],
    );
    assert_success(&pi_install);
    let pi_source = pi_home.join("extensions/project-brain/index.ts");
    let pi_module = root.join("pi-project-brain.mjs");
    fs::copy(pi_source, &pi_module).unwrap();
    run_adapter_runtime_harness(&node, &root, "pi", &pi_module, &project);

    let opencode_home = root.join("opencode home");
    let opencode_install = run(
        &binary(),
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "--opencode-home",
            opencode_home.to_str().unwrap(),
            "install-hooks",
            "opencode",
        ],
        &project,
        None,
        &[],
    );
    assert_success(&opencode_install);
    run_adapter_runtime_harness(
        &node,
        &root,
        "opencode",
        &opencode_home.join("plugins/project-brain.js"),
        &project,
    );

    let dsh_home = root.join("dsh home");
    let fake_dsh = compile_fake_dsh(&root).expect("rustc is required by Rust tests");
    let dsh_install = run(
        &binary(),
        &[
            "--install-root",
            install_root.to_str().unwrap(),
            "--dsh-home",
            dsh_home.to_str().unwrap(),
            "--dsh-profile",
            "project-brain-runtime",
            "install-hooks",
            "dsh",
        ],
        &project,
        None,
        &[("PROJECT_BRAIN_DSH_EXECUTABLE", fake_dsh.as_path())],
    );
    assert_success(&dsh_install);
    run_adapter_runtime_harness(
        &node,
        &root,
        "dsh",
        &dsh_home.join(
            "profiles/project-brain-runtime/node_modules/@project-brain/dsh-plugin/lib/index.js",
        ),
        &project,
    );
    let audit = run(&binary(), &["audit", "--limit", "100"], &project, None, &[]);
    assert_success(&audit);
    let audit: Value = serde_json::from_slice(&audit.stdout).unwrap();
    assert!(
        audit["adapter_events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["adapter_kind"] == "dsh" && event["event_kind"] == "context_requested"
            })
    );
}
