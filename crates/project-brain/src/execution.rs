use std::{
    ffi::OsString,
    io,
    path::Path,
    pin::Pin,
    process::ExitStatus,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};

use processkit::{Command, Outcome, OutputBufferPolicy, ProcessGroup, ProcessGroupOptions};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWrite;

use crate::error::AppError;

pub(crate) const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_STDIN_BYTES: usize = 16 * 1024 * 1024;

pub(crate) struct ProcessResult {
    pub status: ExitStatus,
    pub timed_out: bool,
    pub duration: Duration,
    pub stdout: CapturedOutput,
    pub stderr: CapturedOutput,
}

pub(crate) struct CapturedOutput {
    pub bytes: Vec<u8>,
    pub total_bytes: usize,
    pub sha256: String,
    pub truncated: bool,
}

#[derive(Clone)]
struct CaptureWriter {
    state: Arc<Mutex<CaptureState>>,
}

struct CaptureState {
    bytes: Vec<u8>,
    total_bytes: usize,
    digest: Sha256,
}

impl CaptureWriter {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(CaptureState {
                bytes: Vec::new(),
                total_bytes: 0,
                digest: Sha256::new(),
            })),
        }
    }

    fn snapshot(&self) -> Result<CapturedOutput, AppError> {
        let state = self
            .state
            .lock()
            .map_err(|_| AppError::Provider("外部进程输出捕获状态已损坏".to_owned()))?;
        Ok(CapturedOutput {
            truncated: state.total_bytes > state.bytes.len(),
            bytes: state.bytes.clone(),
            total_bytes: state.total_bytes,
            sha256: format!("{:x}", state.digest.clone().finalize()),
        })
    }
}

impl AsyncWrite for CaptureWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let Ok(mut state) = self.state.lock() else {
            return Poll::Ready(Err(io::Error::other("外部进程输出捕获状态已损坏")));
        };
        state.total_bytes = state.total_bytes.saturating_add(buffer.len());
        state.digest.update(buffer);
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(state.bytes.len());
        state
            .bytes
            .extend_from_slice(&buffer[..buffer.len().min(remaining)]);
        Poll::Ready(Ok(buffer.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_contained(
    executable: &Path,
    launcher_script: Option<&Path>,
    arguments: &[String],
    cwd: &Path,
    timeout: Duration,
    environment: &[(OsString, OsString)],
    observe_timeout: bool,
) -> Result<ProcessResult, AppError> {
    run_contained_with_input(
        executable,
        launcher_script,
        arguments,
        cwd,
        timeout,
        environment,
        observe_timeout,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_contained_with_input(
    executable: &Path,
    launcher_script: Option<&Path>,
    arguments: &[String],
    cwd: &Path,
    timeout: Duration,
    environment: &[(OsString, OsString)],
    observe_timeout: bool,
    stdin_bytes: Option<&[u8]>,
) -> Result<ProcessResult, AppError> {
    if stdin_bytes.is_some_and(|input| input.len() > MAX_STDIN_BYTES) {
        return Err(AppError::Provider(format!(
            "拒绝向外部进程写入超过 {MAX_STDIN_BYTES} 字节的标准输入"
        )));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| AppError::Provider(format!("无法建立外部执行异步运行时：{error}")))?;
    runtime.block_on(run_contained_async(
        executable,
        launcher_script,
        arguments,
        cwd,
        timeout,
        environment,
        observe_timeout,
        stdin_bytes,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_contained_async(
    executable: &Path,
    launcher_script: Option<&Path>,
    arguments: &[String],
    cwd: &Path,
    timeout: Duration,
    environment: &[(OsString, OsString)],
    observe_timeout: bool,
    stdin_bytes: Option<&[u8]>,
) -> Result<ProcessResult, AppError> {
    let stdout_capture = CaptureWriter::new();
    let stderr_capture = CaptureWriter::new();
    let mut argv = Vec::<OsString>::new();
    if let Some(script) = launcher_script {
        argv.push(OsString::from(crate::provider::provider_cli_path(script)));
    }
    argv.extend(arguments.iter().map(OsString::from));

    let mut command = Command::new(executable.as_os_str())
        .args(&argv)
        .current_dir(cwd)
        .env_clear()
        .envs(
            environment
                .iter()
                .map(|(name, value)| (name.as_os_str(), value.as_os_str())),
        )
        .timeout(timeout)
        .timeout_grace(Duration::ZERO)
        .output_buffer(OutputBufferPolicy::bounded(0).with_max_bytes(MAX_CAPTURE_BYTES))
        .stdout_raw_tee(stdout_capture.clone())
        .stderr_raw_tee(stderr_capture.clone());
    if stdin_bytes.is_some() {
        command = command.keep_stdin_open();
    }

    let group =
        ProcessGroup::with_options(ProcessGroupOptions::default().shutdown_timeout(Duration::ZERO))
            .map_err(|error| {
                AppError::Provider(format!(
                    "无法建立外部执行进程树隔离，拒绝启动 {}：{error}",
                    executable.display()
                ))
            })?;
    let started = Instant::now();
    let mut process = group.start(&command).await.map_err(|error| {
        AppError::Provider(format!(
            "无法在进程树隔离中启动 Provider {}：{error}",
            executable.display()
        ))
    })?;
    if let Some(input) = stdin_bytes {
        let mut stdin = process
            .take_stdin()
            .ok_or_else(|| AppError::Provider("外部进程未提供已请求的标准输入管道".to_owned()))?;
        stdin
            .write(input)
            .await
            .map_err(|error| AppError::Provider(format!("无法写入外部进程标准输入：{error}")))?;
        stdin
            .finish()
            .await
            .map_err(|error| AppError::Provider(format!("无法关闭外部进程标准输入：{error}")))?;
    }
    let events = process
        .events()
        .map_err(|error| AppError::Provider(format!("无法观察外部进程退出边界：{error}")))?;
    drop(events);
    let (_, outcome) = processkit::wait_any(&mut [&mut process])
        .await
        .map_err(|error| AppError::Provider(format!("外部进程执行失败：{error}")))?;
    group.shutdown_ref().await.map_err(|error| {
        AppError::Provider(format!("外部进程退出后无法确认其进程树已清空：{error}"))
    })?;
    let finished = process
        .finish()
        .await
        .map_err(|error| AppError::Provider(format!("外部进程输出收尾失败：{error}")))?;
    if finished.outcome != outcome {
        return Err(AppError::Provider(
            "外部进程退出状态在进程树清理前后不一致".to_owned(),
        ));
    }

    let timed_out = outcome.timed_out();
    if timed_out && !observe_timeout {
        return Err(AppError::Provider(format!(
            "Provider 超时（{} 秒）并已终止完整进程树",
            timeout.as_secs()
        )));
    }
    drop(command);
    Ok(ProcessResult {
        status: exit_status(outcome),
        timed_out,
        duration: started.elapsed(),
        stdout: stdout_capture.snapshot()?,
        stderr: stderr_capture.snapshot()?,
    })
}

#[cfg(unix)]
fn exit_status(outcome: Outcome) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt as _;

    if let Some(code) = outcome.code() {
        ExitStatus::from_raw(code << 8)
    } else {
        ExitStatus::from_raw(outcome.signal().unwrap_or(9))
    }
}

#[cfg(target_os = "windows")]
fn exit_status(outcome: Outcome) -> ExitStatus {
    use std::os::windows::process::ExitStatusExt as _;

    ExitStatus::from_raw(outcome.code().unwrap_or(1).cast_unsigned())
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::{OsStr, OsString},
        fs,
        path::{Path, PathBuf},
        process::Command,
        thread,
        time::{Duration, SystemTime},
    };

    use super::{CaptureWriter, MAX_CAPTURE_BYTES, run_contained};
    use sha2::{Digest as _, Sha256};
    use tokio::io::AsyncWriteExt as _;

    const HELPER_ROLE: &str = "PROJECT_BRAIN_CONTAINMENT_HELPER_ROLE";
    const HELPER_READY: &str = "PROJECT_BRAIN_CONTAINMENT_HELPER_READY";
    const HELPER_ESCAPE: &str = "PROJECT_BRAIN_CONTAINMENT_HELPER_ESCAPE";
    const HELPER_ESCAPE_DELAY_MS: &str = "PROJECT_BRAIN_CONTAINMENT_ESCAPE_DELAY_MS";

    fn temp_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-containment-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn helper_arguments() -> Vec<String> {
        vec![
            "--exact".to_owned(),
            "execution::tests::contained_process_helper".to_owned(),
            "--nocapture".to_owned(),
        ]
    }

    fn helper_environment(
        role: &str,
        ready: &Path,
        escape: &Path,
        escape_delay_ms: u64,
    ) -> Vec<(OsString, OsString)> {
        let mut environment = std::env::vars_os().collect::<Vec<_>>();
        environment.push((OsString::from(HELPER_ROLE), OsString::from(role)));
        environment.push((
            OsString::from(HELPER_READY),
            ready.as_os_str().to_os_string(),
        ));
        environment.push((
            OsString::from(HELPER_ESCAPE),
            escape.as_os_str().to_os_string(),
        ));
        environment.push((
            OsString::from(HELPER_ESCAPE_DELAY_MS),
            OsString::from(escape_delay_ms.to_string()),
        ));
        environment
    }

    fn wait_for_path(path: &Path, timeout: Duration) -> bool {
        let started = std::time::Instant::now();
        while started.elapsed() < timeout {
            if path.is_file() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    #[allow(
        clippy::zombie_processes,
        reason = "夹具必须让根进程不等待子进程退出，以证明外层进程树容器负责收敛"
    )]
    fn contained_process_helper() {
        let Some(role) = std::env::var_os(HELPER_ROLE) else {
            return;
        };
        let ready = PathBuf::from(std::env::var_os(HELPER_READY).unwrap());
        let escape = PathBuf::from(std::env::var_os(HELPER_ESCAPE).unwrap());

        if role == OsStr::new("descendant") {
            fs::write(&ready, b"ready\n").unwrap();
            let delay = std::env::var(HELPER_ESCAPE_DELAY_MS)
                .unwrap()
                .parse::<u64>()
                .unwrap();
            thread::sleep(Duration::from_millis(delay));
            fs::write(&escape, b"escaped\n").unwrap();
            return;
        }

        let mut descendant = Command::new(std::env::current_exe().unwrap());
        descendant
            .args(helper_arguments())
            .env(HELPER_ROLE, "descendant")
            .env(HELPER_READY, &ready)
            .env(HELPER_ESCAPE, &escape)
            .env(
                HELPER_ESCAPE_DELAY_MS,
                std::env::var_os(HELPER_ESCAPE_DELAY_MS).unwrap(),
            );
        let _child = descendant.spawn().unwrap();
        assert!(wait_for_path(&ready, Duration::from_secs(5)));
        if role == OsStr::new("timeout-root") {
            thread::sleep(Duration::from_secs(20));
        }
    }

    #[test]
    fn capture_writer_bounds_bytes_but_hashes_the_complete_stream() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let capture = CaptureWriter::new();
        let input = vec![b'x'; MAX_CAPTURE_BYTES + 17];
        runtime.block_on(async {
            let mut writer = capture.clone();
            writer.write_all(&input).await.unwrap();
        });

        let snapshot = capture.snapshot().unwrap();
        assert_eq!(snapshot.bytes.len(), MAX_CAPTURE_BYTES);
        assert_eq!(snapshot.total_bytes, MAX_CAPTURE_BYTES + 17);
        assert!(snapshot.truncated);
        assert_eq!(snapshot.sha256, format!("{:x}", Sha256::digest(&input)));
    }

    #[test]
    fn normal_parent_exit_clears_descendants_before_returning() {
        let root = temp_root("normal-exit");
        let ready = root.join("ready");
        let escape = root.join("escape");
        let executable = std::env::current_exe().unwrap();
        let result = run_contained(
            &executable,
            None,
            &helper_arguments(),
            &root,
            Duration::from_secs(10),
            &helper_environment("normal-root", &ready, &escape, 1500),
            false,
        )
        .unwrap();

        assert!(result.status.success());
        assert!(ready.is_file());
        thread::sleep(Duration::from_secs(2));
        assert!(!escape.exists(), "正常退出后仍有子孙进程存活");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn timeout_clears_the_complete_process_tree_before_returning() {
        let root = temp_root("timeout");
        let ready = root.join("ready");
        let escape = root.join("escape");
        let executable = std::env::current_exe().unwrap();
        let result = run_contained(
            &executable,
            None,
            &helper_arguments(),
            &root,
            Duration::from_secs(2),
            &helper_environment("timeout-root", &ready, &escape, 3500),
            true,
        )
        .unwrap();

        assert!(result.timed_out);
        assert!(ready.is_file());
        thread::sleep(Duration::from_secs(2));
        assert!(!escape.exists(), "超时返回后仍有子孙进程存活");
        fs::remove_dir_all(root).unwrap();
    }
}
