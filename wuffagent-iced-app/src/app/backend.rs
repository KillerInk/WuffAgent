use std::sync::Arc;

use tokio::sync::broadcast;

use crate::client::ChatClient;
use crate::config::Config;
use crate::server::ServerManager;
use crate::tools::ToolManager;
use crate::agents::AgentEngine;
use crate::types::AppEvent;

/// Shared backend state passed to the UI layer.
///
/// Holds the tokio runtime handle for background work,
/// the broadcast sender for UI events, and all core services.
#[derive(Clone)]
pub struct Backend {
    pub runtime: Arc<tokio::runtime::Handle>,
    pub event_sender: broadcast::Sender<AppEvent>,
    pub client: Arc<std::sync::Mutex<ChatClient>>,
    pub server: Arc<ServerManager>,
    pub config: Arc<std::sync::Mutex<Config>>,
    pub tool_manager: Arc<ToolManager>,
    pub agent_engine: Arc<AgentEngine>,
}

impl Backend {
    /// Spawn a future onto the background tokio runtime.
    pub fn spawn<F: std::future::Future + Send + 'static>(&self, f: F) -> tokio::task::JoinHandle<F::Output>
    where
        F::Output: Send + 'static,
    {
        self.runtime.spawn(f)
    }

    /// Create a new Backend from the bootstrap values.
    pub fn new(
        runtime: Arc<tokio::runtime::Handle>,
        client: Arc<std::sync::Mutex<ChatClient>>,
        server: Arc<ServerManager>,
        config: Arc<std::sync::Mutex<Config>>,
        tool_manager: Arc<ToolManager>,
        agent_engine: Arc<AgentEngine>,
    ) -> Self {
        let (event_sender, _) = broadcast::channel(64);
        Backend {
            runtime,
            event_sender,
            client,
            server,
            config,
            tool_manager,
            agent_engine,
        }
    }
}
