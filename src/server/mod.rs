use tokio::process::Command;
use tokio::sync::Mutex;
use std::sync::Arc;
use std::time::Duration;
use std::net::TcpStream;
use tokio::io::AsyncBufReadExt;

pub struct ServerManager {
    server_path: String,
    model_path: String,
    port: u16,
    n_gpu_layers: i32,
    n_ctx: u32,
    threads: u32,
    process: Arc<Mutex<Option<tokio::process::Child>>>,
    running: Arc<std::sync::atomic::AtomicBool>,
    error: Arc<std::sync::Mutex<Option<String>>>,
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
            error: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub async fn start_server(&self) -> Result<(), Error> {
        self.start_server_with_paths(&self.server_path, &self.model_path, self.port, self.n_gpu_layers, self.n_ctx, self.threads).await
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
        // Build arguments
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
            "--n-ctx".to_string(),
            n_ctx.to_string(),
        ];

        let mut cmd = Command::new(server_path);
        cmd.args(&args);

        // Capture stdout and stderr for monitoring
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let child = cmd.spawn().map_err(|e| Error::SpawnFailed(e, server_path.to_string()))?;

        *self.process.lock().await = Some(child);
        self.running.store(true, std::sync::atomic::Ordering::SeqCst);

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
                Ok(_) => {}
                Err(_) => {}
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
mod tests {
    use super::*;

    #[test]
    fn test_server_manager_creation() {
        let server = ServerManager::new(
            "llama-server",
            "test_model.gguf",
            18080,
            0,
            2048,
            4,
        );
        assert!(!server.is_running());
        assert_eq!(server.get_error(), None);
    }

    #[test]
    fn test_parse_progress() {
        assert_eq!(ServerManager::parse_progress("loading model ... 100%"), Some(100.0));
        assert_eq!(ServerManager::parse_progress("loading model ... 50%"), Some(50.0));
        assert_eq!(ServerManager::parse_progress("loading model ... 75.5%"), Some(75.5));
        assert_eq!(ServerManager::parse_progress("no percentage here"), None);
        assert_eq!(ServerManager::parse_progress(""), None);
        assert_eq!(ServerManager::parse_progress("100"), None);
    }
}
