//! dsh kernel lifecycle: install the bundled kernel, spawn `dsh web`, supervise
//! the process (restart on crash), and support kernel online updates from the
//! npm registry + release artifacts.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// The dsh version bundled with this build (injected by build.rs).
pub const KERNEL_VERSION: &str = env!("DSH_KERNEL_VERSION");
/// The npm package name of the dsh kernel.
pub const KERNEL_PACKAGE: &str = "@deepseek-ai/dsh";

const READY_TIMEOUT: Duration = Duration::from_secs(90);
const RESTART_DELAY: Duration = Duration::from_secs(2);
const MAX_RESTARTS: u32 = 3;
const HEALTH_RETRIES: u32 = 8;
const HEALTH_INTERVAL: Duration = Duration::from_millis(600);
const KILL_GRACE: Duration = Duration::from_secs(3);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

const NPM_REGISTRY: &str = "https://registry.npmjs.org";
/// Release channel that hosts self-contained kernel tarballs
/// (`kernel-<version>/kernel-<version>.tar.gz`).
const KERNEL_RELEASE_BASE: &str =
    "https://github.com/GotoFox/deepseek-harness-client/releases/download";

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct KernelStatus {
    pub phase: String,
    pub port: Option<u16>,
    pub error: Option<String>,
    pub version: String,
    pub app_version: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Installing,
    Starting,
    Ready,
    Restarting,
    Error,
    Stopped,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Installing => "installing",
            Phase::Starting => "starting",
            Phase::Ready => "ready",
            Phase::Restarting => "restarting",
            Phase::Error => "error",
            Phase::Stopped => "stopped",
        }
    }
}

struct Inner {
    phase: Phase,
    port: Option<u16>,
    error: Option<String>,
    version: String,
    pid: Option<u32>,
}

pub struct KernelManager {
    inner: Arc<Mutex<Inner>>,
}

impl Default for KernelManager {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                phase: Phase::Idle,
                port: None,
                error: None,
                version: KERNEL_VERSION.to_string(),
                pid: None,
            })),
        }
    }

    pub fn status(&self) -> KernelStatus {
        let i = self.inner.lock().unwrap();
        KernelStatus {
            phase: i.phase.as_str().to_string(),
            port: i.port,
            error: i.error.clone(),
            version: i.version.clone(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Launch the kernel supervision loop on a background thread.
    pub fn start(&self, app: AppHandle) {
        let inner = self.inner.clone();
        thread::spawn(move || {
            let _ = run_loop(&app, &inner);
        });
    }

    /// Stop the kernel: mark stopped and terminate the child process group.
    pub fn stop(&self) {
        let pid = {
            let mut i = self.inner.lock().unwrap();
            i.phase = Phase::Stopped;
            i.port = None;
            i.pid
        };
        if let Some(pid) = pid {
            kill_pid(pid);
        }
    }

    /// Stop and relaunch the kernel from scratch.
    pub fn retry(&self, app: AppHandle) {
        self.stop();
        {
            let mut i = self.inner.lock().unwrap();
            i.phase = Phase::Idle;
            i.error = None;
        }
        self.start(app);
    }

    /// Background thread: check npm for a newer dsh, download the matching
    /// kernel tarball from the release channel, install it and restart the
    /// kernel. Results are surfaced via the `kernel-update-result` event.
    pub fn check_kernel_update(&self, app: AppHandle) {
        let inner = self.inner.clone();
        thread::spawn(move || {
            let result = do_kernel_update(&app, &inner);
            let _ = app.emit("kernel-update-result", result);
        });
    }
}

fn run_loop(app: &AppHandle, inner: &Arc<Mutex<Inner>>) -> Result<(), String> {
    {
        let mut i = inner.lock().unwrap();
        i.phase = Phase::Installing;
    }
    let kernel_dir = install_kernel(app)?;
    let version = kernel_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    {
        let mut i = inner.lock().unwrap();
        i.version = version;
    }

    let mut crashes = 0u32;
    loop {
        {
            let mut i = inner.lock().unwrap();
            i.phase = Phase::Starting;
            i.port = None;
            i.pid = None;
            i.error = None;
        }
        let outcome = run_once(app, inner, &kernel_dir);
        let stopped = inner.lock().unwrap().phase == Phase::Stopped;
        if stopped {
            return Ok(());
        }
        crashes += 1;
        let message = match &outcome {
            Ok(()) => "dsh 进程意外退出".to_string(),
            Err(e) => e.clone(),
        };
        if crashes >= MAX_RESTARTS {
            {
                let mut i = inner.lock().unwrap();
                i.phase = Phase::Error;
                i.error = Some(format!("dsh 多次启动失败：{message}"));
            }
            emit_status(app, inner);
            return Err(message);
        }
        {
            let mut i = inner.lock().unwrap();
            i.phase = Phase::Restarting;
            i.error = Some(message);
        }
        emit_status(app, inner);
        thread::sleep(RESTART_DELAY);
    }
}

/// One full dsh run: spawn, wait for the readiness line, health-check, point
/// the window at it, then block until the child exits.
fn run_once(app: &AppHandle, inner: &Arc<Mutex<Inner>>, kernel_dir: &Path) -> Result<(), String> {
    let node = node_path(app)?;
    let entry = kernel_dir.join("node_modules/@deepseek-ai/dsh/lib/bin.js");
    if !entry.exists() {
        return Err(format!("dsh entry missing: {}", entry.display()));
    }

    let mut cmd = Command::new(&node);
    cmd.arg(&entry)
        .args(["--profile", "web", "--host", "127.0.0.1", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group so we can terminate agent-spawned children too.
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| format!("spawn node failed: {e}"))?;
    let pid = child.id();
    {
        let mut i = inner.lock().unwrap();
        i.pid = Some(pid);
    }

    let log_dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    let sink = LogSink::new(log_dir);
    let (tx, rx) = mpsc::sync_channel::<u16>(1);

    let out = child.stdout.take().expect("stdout is piped");
    let err = child.stderr.take().expect("stderr is piped");
    {
        let sink = sink.clone();
        let tx = tx.clone();
        thread::spawn(move || pipe_lines(out, sink, Some(tx)));
    }
    thread::spawn(move || pipe_lines(err, sink, None));

    let port = match rx.recv_timeout(READY_TIMEOUT) {
        Ok(p) => p,
        Err(_) => {
            let _ = child.kill();
            return Err("dsh 未在 90 秒内输出就绪信号".to_string());
        }
    };

    let url = format!("http://127.0.0.1:{port}/");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let mut healthy = false;
    for _ in 0..HEALTH_RETRIES {
        if client
            .get(&url)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            healthy = true;
            break;
        }
        thread::sleep(HEALTH_INTERVAL);
    }
    if !healthy {
        let _ = child.kill();
        return Err(format!("dsh 健康检查失败：{url}"));
    }

    {
        let mut i = inner.lock().unwrap();
        i.phase = Phase::Ready;
        i.port = Some(port);
        i.error = None;
    }
    emit_status(app, inner);

    if let Some(w) = app.get_webview_window("main") {
        if let Ok(u) = tauri::Url::parse(&url) {
            let _ = w.navigate(u);
        }
    }

    let _ = child.wait();
    Ok(())
}

/// Extract the dsh install root: prefer the version pinned in CURRENT, falling
/// back to (and installing) the bundled kernel version.
fn install_kernel(app: &AppHandle) -> Result<PathBuf, String> {
    let data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let kernel_root = data.join("kernel");
    fs::create_dir_all(&kernel_root).map_err(|e| e.to_string())?;

    let current_file = kernel_root.join("CURRENT");
    let mut version = KERNEL_VERSION.to_string();
    if let Ok(c) = fs::read_to_string(&current_file) {
        let v = c.trim().to_string();
        if !v.is_empty() && kernel_root.join(&v).is_dir() {
            version = v;
        }
    }

    let version_dir = kernel_root.join(&version);
    if !version_dir.join("node_modules/@deepseek-ai/dsh/lib/bin.js").exists() {
        let src = app
            .path()
            .resource_dir()
            .map_err(|e| e.to_string())?
            .join("kernel.tar.gz");
        if !src.exists() {
            return Err(format!("bundled kernel resource missing: {}", src.display()));
        }
        let tmp = kernel_root.join(format!("{version}.tmp"));
        if tmp.exists() {
            fs::remove_dir_all(&tmp).map_err(|e| e.to_string())?;
        }
        if version_dir.exists() {
            fs::remove_dir_all(&version_dir).map_err(|e| e.to_string())?;
        }
        fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;

        let gz = File::open(&src).map_err(|e| e.to_string())?;
        let decoder = flate2::read::GzDecoder::new(gz);
        let mut archive = tar::Archive::new(decoder);
        archive
            .unpack(&tmp)
            .map_err(|e| format!("解压内核失败: {e}"))?;
        fs::rename(&tmp, &version_dir).map_err(|e| e.to_string())?;
        fs::write(&current_file, format!("{version}\n")).map_err(|e| e.to_string())?;
    }

    Ok(version_dir)
}

fn node_path(app: &AppHandle) -> Result<PathBuf, String> {
    let res = app.path().resource_dir().map_err(|e| e.to_string())?;
    let p = if cfg!(target_os = "windows") {
        res.join("node").join("win").join("node.exe")
    } else {
        res.join("node").join("node")
    };
    if p.exists() {
        Ok(p)
    } else {
        Err(format!("node runtime missing: {}", p.display()))
    }
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
        thread::sleep(KILL_GRACE);
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/pid", &pid.to_string(), "/T", "/F"])
            .status();
    }
}

fn pipe_lines<R: Read + Send + 'static>(
    reader: R,
    sink: LogSink,
    port_tx: Option<mpsc::SyncSender<u16>>,
) {
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else { break };
        sink.write(line.as_bytes());
        if let Some(tx) = &port_tx {
            if let Some(port) = extract_port(&line) {
                let _ = tx.try_send(port);
            }
        }
    }
}

/// Parse the readiness line dsh prints once its HTTP server is bound:
/// `dsh web: http://127.0.0.1:51315 (LAN: ...)`.
fn extract_port(line: &str) -> Option<u16> {
    const PREFIX: &str = "dsh web: http://127.0.0.1:";
    let start = line.find(PREFIX)?;
    let rest = &line[start + PREFIX.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[derive(Clone)]
struct LogSink {
    file: Arc<Mutex<File>>,
}

impl LogSink {
    fn new(dir: PathBuf) -> Self {
        fs::create_dir_all(&dir).ok();
        let path = dir.join(format!("dsh-{}.log", unix_seconds()));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|_| File::create(&path).expect("log file"));
        Self {
            file: Arc::new(Mutex::new(file)),
        }
    }

    fn write(&self, line: &[u8]) {
        let mut f = self.file.lock().unwrap();
        let _ = f.write_all(line);
        let _ = f.write_all(b"\n");
    }
}

fn unix_seconds() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn emit_status(app: &AppHandle, inner: &Arc<Mutex<Inner>>) {
    let i = inner.lock().unwrap();
    let payload = KernelStatus {
        phase: i.phase.as_str().to_string(),
        port: i.port,
        error: i.error.clone(),
        version: i.version.clone(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    drop(i);
    let _ = app.emit("kernel-status", payload);
}

// ── kernel online update ─────────────────────────────────────────────────────

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())
}

fn npm_latest_version() -> Result<String, String> {
    let client = http_client()?;
    let resp = client
        .get(format!("{NPM_REGISTRY}/{KERNEL_PACKAGE}/latest"))
        .send()
        .map_err(|e| format!("查询 npm 版本失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("npm 返回 {}", resp.status()));
    }
    let json: serde_json::Value = serde_json::from_reader(resp).map_err(|e| e.to_string())?;
    json.get("version")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "npm 响应缺少 version 字段".to_string())
}

fn do_kernel_update(app: &AppHandle, inner: &Arc<Mutex<Inner>>) -> KernelUpdateResult {
    let current = inner.lock().unwrap().version.clone();
    let latest = match npm_latest_version() {
        Ok(v) => v,
        Err(e) => return KernelUpdateResult::fail(&current, &e),
    };
    if latest == current {
        return KernelUpdateResult::ok(
            &current,
            format!("已是最新内核 dsh v{current}"),
            false,
        );
    }

    let data = match app.path().app_data_dir() {
        Ok(d) => d,
        Err(e) => return KernelUpdateResult::fail(&current, &e.to_string()),
    };
    let kernel_root = data.join("kernel");
    let url = format!("{KERNEL_RELEASE_BASE}/kernel-{latest}/kernel-{latest}.tar.gz");
    let client = match http_client() {
        Ok(c) => c,
        Err(e) => return KernelUpdateResult::fail(&current, &e),
    };
    let _ = app.emit("kernel-update-progress", format!("正在下载 dsh v{latest}…"));
    let resp = match client.get(&url).send() {
        Ok(r) => r,
        Err(e) => {
            return KernelUpdateResult::fail(
                &current,
                &format!("下载内核包失败：{e}（内核包需先在发布渠道构建）"),
            )
        }
    };
    if !resp.status().is_success() {
        return KernelUpdateResult::fail(
            &current,
            &format!("内核包尚未发布（HTTP {}）", resp.status()),
        );
    }

    // Download to a temp file, install into a fresh version dir, then flip CURRENT.
    let tmp_gz = kernel_root.join(format!("kernel-{latest}.tar.gz.tmp"));
    if let Err(e) = resp
        .bytes()
        .map_err(|e| e.to_string())
        .and_then(|bytes| fs::write(&tmp_gz, bytes).map_err(|e| e.to_string()))
    {
        return KernelUpdateResult::fail(&current, &e);
    }

    let version_dir = kernel_root.join(&latest);
    let tmp_dir = kernel_root.join(format!("{latest}.tmp"));
    let install = (|| -> Result<(), String> {
        if tmp_dir.exists() {
            fs::remove_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
        }
        if version_dir.exists() {
            fs::remove_dir_all(&version_dir).map_err(|e| e.to_string())?;
        }
        fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
        let gz = File::open(&tmp_gz).map_err(|e| e.to_string())?;
        let decoder = flate2::read::GzDecoder::new(gz);
        tar::Archive::new(decoder)
            .unpack(&tmp_dir)
            .map_err(|e| format!("解压内核失败：{e}"))?;
        fs::rename(&tmp_dir, &version_dir).map_err(|e| e.to_string())?;
        fs::write(kernel_root.join("CURRENT"), format!("{latest}\n")).map_err(|e| e.to_string())?;
        fs::remove_file(&tmp_gz).ok();
        Ok(())
    })();
    if let Err(e) = install {
        fs::remove_dir_all(&tmp_dir).ok();
        return KernelUpdateResult::fail(&current, &e);
    }

    // Restart the kernel onto the new version.
    {
        let mut i = inner.lock().unwrap();
        i.version = latest.clone();
    }
    {
        let km = app.state::<KernelManager>();
        km.retry(app.clone());
    }
    KernelUpdateResult::ok(&latest, format!("内核已更新为 dsh v{latest}"), true)
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct KernelUpdateResult {
    pub ok: bool,
    pub version: String,
    pub message: String,
    pub restarting: bool,
}

impl KernelUpdateResult {
    fn ok(version: &str, message: String, restarting: bool) -> Self {
        Self {
            ok: true,
            version: version.to_string(),
            message,
            restarting,
        }
    }

    fn fail(version: &str, message: &str) -> Self {
        Self {
            ok: false,
            version: version.to_string(),
            message: message.to_string(),
            restarting: false,
        }
    }
}
