# Phase 3: Server Manager (Rust)

## Status: Pending

---

### Step 3.1: Process Lifecycle

**Objective**: Start and stop llama-server process using tokio.

**Tasks**:
- Create `src/server/mod.rs`
- Implement `ServerManager` struct
- `start_server()` spawns `llama-server` with config params via `tokio::process::Command`
- `stop_server()` sends SIGTERM, waits for exit
- `is_running()` checks process state
- `wait_for_ready()` polls HTTP endpoint

```rust
use tokio::process::Command;
use tokio::sync::Mutex;
use std::sync::Arc;
use std::time::Duration;
use std::net::TcpStream;

pub struct ServerManager {
    server_path: String,
    model_path: String,
    port: u16,
    n_gpu_layers: i32,
    n_ctx: u32,
    threads: u32,
    process: Arc<Mutex<Option<tokio::process::Child>>>,
    running: Arc<std::sync::atomic::AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ServerStatus {
    Stopped,
    Starting,
    Ready,
    Generating,
    Error(String),
}

impl ServerManager {
    pub fn new(server_path: &str, model_path: &str, port: u16, n_gpu_layers: i32, n_ctx: u32, threads: u32) -> Self {
        Self {
            server_path: server_path.to_string(),
            model_path: model_path.to_string(),
            port,
            n_gpu_layers,
            n_ctx,
            threads,
            process: Arc::new(Mutex::new(None)),
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            error: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn start_server(&self) -> Result<(), Error> {
        // Build arguments
        let args = vec![
            "--model".to_string(),
            self.model_path.clone(),
            "--port".to_string(),
            self.port.to_string(),
            "--host".to_string(),
            "127.0.0.1".to_string(),
            "--threads".to_string(),
            self.threads.to_string(),
            "--n-gpu-layers".to_string(),
            self.n_gpu_layers.to_string(),
            "--n_ctx".to_string(),
            self.n_ctx.to_string(),
        ];

        let mut cmd = Command::new(&self.server_path);
        cmd.args(&args);
        
        // Capture stdout and stderr for monitoring
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let child = cmd.spawn().map_err(|e| Error::SpawnFailed(e, self.server_path.clone()))?;
        
        *self.process.lock().await = Some(child);
        self.running.store(true, std::sync::atomic::Ordering::SeqCst);

        // Spawn monitor task
        let running = self.running.clone();
        let process = self.process.clone();
        let error = self.error.clone();
        tokio::spawn(async move {
            if let Some(mut c) = process.lock().await.take() {
                match c.wait().await {
                    Ok(status) => {
                        running.store(false, std::sync::atomic::Ordering::SeqCst);
                        if !status.success() {
                            let err = format!("Server exited with status: {}", status);
                            *error.lock().await = Some(err);
                        }
                    }
                    Err(e) => {
                        running.store(false, std::sync::atomic::Ordering::SeqCst);
                        *error.lock().await = Some(e.to_string());
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
            child.kill().await?;
        }
        *process = None;
        self.running.store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::SeqCst)
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
                Ok(_) => {},
                Err(_) => {},
            }
            
            // Also try TCP connection
            if TcpStream::connect(format!("127.0.0.1:{}", self.port)).is_ok() {
                // Give HTTP a moment to be ready
                tokio::time::sleep(Duration::from_millis(500)).await;
                if reqwest::get(format!("http://127.0.0.1:{}/health", self.port)).await.is_ok() {
                    return Ok(());
                }
            }
            
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub fn get_error(&self) -> Option<String> {
        self.error.lock_blocking().clone()
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
```

**Success Criteria**:
- Server process starts with correct arguments
- Process is running after `start_server()`
- Server stops after `stop_server()`
- `is_running()` returns correct boolean
- `wait_for_ready()` returns true when server responds

**Dependencies**: Step 2.1 (config available)

---

### Step 3.2: Process Monitoring

**Objective**: Detect server crashes, handle errors.

**Tasks**:
- Spawn task to monitor stdout/stderr
- Log server output to tracing
- Expose error channel for UI updates

```rust
impl ServerManager {
    pub async fn monitor_output(&self) -> tokio::sync::mpsc::Receiver<String> {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let process = self.process.lock().await.clone();
        
        if let Some(mut child) = process {
            tokio::spawn(async move {
                if let Some(stdout) = child.stdout.take() {
                    let reader = tokio::io::BufReader::new(stdout);
                    let mut lines = tokio::io::AsyncBufReadExt::lines(reader);
                    while let Some(line) = lines.next_line().await.unwrap_or_default() {
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
```

**Success Criteria**:
- Crash detection works
- Error messages are visible
- Process state is tracked

**Dependencies**: Step 3.1

---

### Step 3.3: Model Loading Progress

**Objective**: Show loading progress during model loading.

**Tasks**:
- Parse stderr for loading progress messages from llama-server
- Expose progress channel for UI updates
- Extract percentage from output lines

```rust
impl ServerManager {
    pub fn parse_progress(line: &str) -> Option<f32> {
        // llama-server outputs lines like: "loading model ... 100%"
        if let Some(pos) = line.find('%') {
            let before = &line[..pos];
            if let Some(last_space) = before.rfind(' ') {
                if let Ok(pct) = before[last_space + 1..].parse::<f32>() {
                    return Some(pct);
                }
            }
        }
        None
    }
}
```

**Success Criteria**:
- Progress percentage available

**Dependencies**: Step 3.1

---

## Files Created:
- `src/server/mod.rs`

## Dependencies on other phases:
- Phase 2 (config provides paths, port, GPU, threads, n_ctx)
- Phase 5 (UI needs progress indicator)

## Review Notes:
- `tokio::process::Command` is used for async process management
- `Arc<Mutex<Option<Child>>>` for thread-safe process handle
- `Arc<AtomicBool>` for thread-safe running state
- Monitor task detects process exit without blocking main thread
- stdout/stderr monitoring for crash detection and progress
- `wait_for_ready` uses HTTP polling of `/health` endpoint
- `child.kill()` for forceful shutdown
- Progress parsing from stderr (llama-server outputs loading info with %)
- Windows: `child.kill()` works, SIGTERM equivalent
