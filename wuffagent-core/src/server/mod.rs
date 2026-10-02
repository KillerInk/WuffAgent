use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;
use tokio::sync::Mutex;

mod args;
pub use args::{kv_estimate_gb, ServerArgs};
mod progress;
pub use progress::parse_progress;
pub mod status;

pub struct ServerManager {
    server_path: String,
    model_path: String,
    port: u16,
    n_gpu_layers: i32,
    n_ctx: u32,
    threads: u32,
    /// Tunable server CLI arguments (batch sizes, slots, metrics, ...) —
    /// see [`ServerArgs`].
    args: ServerArgs,
    process: Arc<Mutex<Option<tokio::process::Child>>>,
    running: Arc<std::sync::atomic::AtomicBool>,
    error: Arc<std::sync::Mutex<Option<String>>>,
    /// Whether the server status monitor is active.
    monitor_active: Arc<std::sync::atomic::AtomicBool>,
    /// Handle to the server status monitor task (so we can await it on shutdown).
    monitor_handle: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// True when we ATTACHED to a server already listening on the port
    /// (no child process of ours — `stop_server` must not kill it).
    attached: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerStatus {
    Stopped,
    Starting,
    Ready,
    Generating,
    Error(String),
}

impl ServerManager {
    pub fn new(
        server_path: &str,
        model_path: &str,
        port: u16,
        n_gpu_layers: i32,
        n_ctx: u32,
        threads: u32,
        args: ServerArgs,
    ) -> Self {
        Self {
            server_path: server_path.to_string(),
            model_path: model_path.to_string(),
            port,
            n_gpu_layers,
            n_ctx,
            threads,
            args,
            process: Arc::new(Mutex::new(None)),
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            error: Arc::new(std::sync::Mutex::new(None)),
            monitor_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            monitor_handle: Arc::new(std::sync::Mutex::new(None)),
            attached: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Returns a no-op ServerManager that is always stopped.
    /// Used when in remote mode so the UI has a valid ServerManager but no local process is managed.
    pub fn noop() -> Self {
        Self {
            server_path: String::new(),
            model_path: String::new(),
            port: 0,
            n_gpu_layers: 0,
            n_ctx: 0,
            threads: 0,
            args: ServerArgs::default(),
            process: Arc::new(Mutex::new(None)),
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            error: Arc::new(std::sync::Mutex::new(None)),
            monitor_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            monitor_handle: Arc::new(std::sync::Mutex::new(None)),
            attached: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub async fn start_server(&self) -> Result<(), Error> {
        self.start_server_with_paths(
            &self.server_path,
            &self.model_path,
            self.port,
            self.n_gpu_layers,
            self.n_ctx,
            self.threads,
        )
        .await
    }

    /// Start the server status monitor (polls `/slots` + `/props` every 3s).
    ///
    /// Must be called AFTER the server is ready (i.e. after `wait_for_ready`).
    /// The monitor emits `AppEvent::ServerStatus` snapshots via `event_tx`.
    /// Safe to call multiple times — replaces any existing monitor.
    pub fn start_status_monitor(
        &self,
        base_url: &str,
        api_key: Option<&str>,
        event_tx: std::sync::mpsc::Sender<crate::types::AppEvent>,
    ) {
        // Replace any existing monitor WITHOUT blocking (this is called from
        // sync contexts — bootstrap, UI callbacks — where `block_on` would
        // panic: "Cannot start a runtime from within a runtime"). The old
        // task is aborted; the shared `active` flag stays owned by the NEW
        // task (app shutdown clears it via `stop_status_monitor`).
        if let Some(old) = self.monitor_handle.lock().unwrap().take() {
            old.abort();
        }
        self.monitor_active
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let base_url = base_url.to_string();
        let api_key = api_key.map(|s| s.to_string());
        let active = self.monitor_active.clone();

        let handle = status::spawn_server_monitor(base_url, api_key, event_tx, active);

        *self.monitor_handle.lock().unwrap() = Some(handle);
    }

    /// Stop the server status monitor.
    pub async fn stop_status_monitor(&self) {
        self.monitor_active
            .store(false, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut guard) = self.monitor_handle.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.await;
            }
        }
    }

    /// Attach to a server already listening on `port` (attach mode, Phase 2
    /// item 3): marks this manager running/attached WITHOUT spawning a
    /// process. Sync on purpose (atomic state only) so the non-async
    /// bootstrap can call it at startup. Returns true when attached.
    /// `stop_server` on an attached manager only clears the flags — it must
    /// not kill a process that is not ours.
    pub fn attach_if_running(&self, port: u16) -> bool {
        if TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok() {
            self.attached
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.running
                .store(true, std::sync::atomic::Ordering::SeqCst);
            tracing::info!(port, "attached to existing server on :{port} (no process spawned)");
            true
        } else {
            self.attached
                .store(false, std::sync::atomic::Ordering::SeqCst);
            false
        }
    }

    pub async fn start_server_with_paths(
        &self,
        server_path: &str,
        model_path: &str,
        port: u16,
        n_gpu_layers: i32,
        n_ctx: u32,
        threads: u32,
    ) -> Result<(), Error> {
        // Attach mode (Phase 2 item 3): if something is already listening on
        // the port, attach instead of failing with a port collision.
        if self.attach_if_running(port) {
            return Ok(());
        }

        // Build arguments. Core flags first, then the config-driven tuning
        // args (batch sizes, slots, metrics, ...) from `ServerArgs`.
        // NOTE: current llama.cpp builds use `--ctx-size` (NOT `--n-ctx` —
        // unknown arguments are fatal and the server exits 1).
        let args = vec![
            "--model".to_string(),
            model_path.to_string(),
            "--port".to_string(),
            port.to_string(),
            "--host".to_string(),
            "127.0.0.1".to_string(),
            "--threads".to_string(),
            threads.to_string(),
            "--n-gpu-layers".to_string(),
            n_gpu_layers.to_string(),
            "--ctx-size".to_string(),
            n_ctx.to_string(),
        ];

        // KV cache budget warning: parallel slots multiply the per-slot KV
        // cache, the most common way to OOM a local setup.
        if self.args.parallel > 1 {
            let gb = self.args.kv_estimate_gb(n_ctx);
            tracing::info!(
                n_parallel = self.args.parallel,
                n_ctx,
                kv_estimate_gb = format!("{:.1}", gb),
                "parallel slots multiply the KV cache (7B-class estimate); \
                 lower --parallel or n_ctx if the server OOMs on load"
            );
        }

        let mut cmd = Command::new(server_path);
        cmd.args(&args);
        cmd.args(&self.args.to_cli_args());

        // Windows: give the server its own process group (group id = child
        // pid) so `stop_server` can ask it to handle CTRL_C gracefully
        // instead of SIGKILL (Phase 2 item 4).
        #[cfg(windows)]
        cmd.creation_flags(
            windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP,
        );

        // Capture stdout and stderr for monitoring
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let child = cmd
            .spawn()
            .map_err(|e| Error::SpawnFailed(e, server_path.to_string()))?;

        *self.process.lock().await = Some(child);
        self.running
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // Spawn monitor task
        let running = self.running.clone();
        let error = self.error.clone();
        let process = self.process.clone();
        tokio::spawn(async move {
            if let Some(mut c) = process.lock().await.take() {
                match c.wait().await {
                    Ok(status) => {
                        running.store(false, std::sync::atomic::Ordering::SeqCst);
                        if !status.success() {
                            let err = format!("Server exited with status: {}", status);
                            *error.lock().unwrap() = Some(err);
                        }
                    }
                    Err(e) => {
                        running.store(false, std::sync::atomic::Ordering::SeqCst);
                        *error.lock().unwrap() = Some(e.to_string());
                    }
                }
            }
        });

        Ok(())
    }

    pub async fn stop_server(&self) -> Result<(), Error> {
        if !self.is_running() {
            return Ok(());
        }

        let mut process = self.process.lock().await;
        if let Some(child) = process.as_mut() {
            #[cfg(windows)]
            {
                // Graceful shutdown (Phase 2 item 4): ask the server's
                // process group to handle CTRL_C — llama-server registers a
                // console handler that releases model memory cleanly. Wait up
                // to 5 s for the exit, then fall back to a hard kill.
                // (`GenerateConsoleCtrlEvent` can fail when the calling
                // process has no console of its own — e.g. this GUI app; the
                // kill fallback covers that case.)
                if let Some(pid) = child.id() {
                    let sent = unsafe {
                        // SAFETY: `GenerateConsoleCtrlEvent` takes a plain u32
                        // process-group id (the child's pid — a valid group
                        // because we spawned it with CREATE_NEW_PROCESS_GROUP)
                        // and no pointers; failure is reported via its return
                        // value (and falls back to `kill()` below).
                        windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(
                            windows_sys::Win32::System::Console::CTRL_C_EVENT,
                            pid,
                        )
                    };
                    tracing::info!(
                        pid,
                        ctrl_c_sent = sent != 0,
                        "sent CTRL_C to llama-server process group; waiting up to 5s for a clean exit"
                    );
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                    let mut exited = false;
                    while tokio::time::Instant::now() < deadline {
                        match child.try_wait() {
                            Ok(Some(_)) => {
                                exited = true;
                                break;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                tracing::warn!(error = %e, "try_wait failed during graceful shutdown");
                                break;
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    if !exited {
                        tracing::info!(pid, "llama-server did not exit within 5s; killing");
                        child.kill().await?;
                    }
                } else {
                    child.kill().await?;
                }
            }
            #[cfg(not(windows))]
            child.kill().await?;
        }
        *process = None;
        self.running
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Returns the n_ctx value this server was started with.
    /// For local mode this is accurate since we control the server process.
    /// Returns 0 for noop (remote mode) server managers.
    pub fn get_n_ctx(&self) -> u32 {
        self.n_ctx
    }

    /// Returns the actual n_gpu_layers value this server was started with.
    pub fn get_n_gpu_layers(&self) -> i32 {
        self.n_gpu_layers
    }

    /// Returns the actual threads value this server was started with.
    pub fn get_threads(&self) -> u32 {
        self.threads
    }

    /// Returns the tuning arguments this server was started with (Phase 2).
    pub fn get_args(&self) -> &ServerArgs {
        &self.args
    }

    /// Returns true when this manager is attached to a server that was
    /// already listening on its port (attach mode — the process is NOT ours,
    /// so `stop_server` only clears the running flags).
    pub fn is_attached(&self) -> bool {
        self.attached
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Returns the base URL for this server (e.g. `http://127.0.0.1:8080`).
    pub fn get_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub async fn wait_for_ready(&self, timeout: Duration) -> Result<(), Error> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if tokio::time::Instant::now() > deadline {
                return Err(Error::Timeout);
            }

            // Try HTTP health check
            match reqwest::get(format!("http://127.0.0.1:{}/health", self.port)).await {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(_) => {}
                Err(_) => {}
            }

            // Also try TCP connection
            if TcpStream::connect(format!("127.0.0.1:{}", self.port)).is_ok() {
                // Give HTTP a moment to be ready
                tokio::time::sleep(Duration::from_millis(500)).await;
                if reqwest::get(format!("http://127.0.0.1:{}/health", self.port))
                    .await
                    .is_ok()
                {
                    return Ok(());
                }
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub fn get_error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    pub async fn monitor_output(&self) -> tokio::sync::mpsc::Receiver<String> {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let mut process = self.process.lock().await;

        if let Some(child) = process.take() {
            tokio::spawn(async move {
                if let Some(stdout) = child.stdout {
                    let reader = tokio::io::BufReader::new(stdout);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        tracing::info!("[llama-server] {}", line);
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                }
            });
        }

        rx
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Failed to spawn server: {0} at {1}")]
    SpawnFailed(std::io::Error, String),
    #[error("Server not ready within timeout")]
    Timeout,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}

#[cfg(test)]
mod tests;
