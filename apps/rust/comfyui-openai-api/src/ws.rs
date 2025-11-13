use log::{debug, error};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures::stream::{StreamExt, SplitStream};

const COMPLETED_JOBS_CAPACITY: usize = 100;

/// Circular buffer for tracking completed job IDs
struct CompletedJobsBuffer {
    jobs: Vec<String>,
    index: usize,
    is_full: bool,
}

impl CompletedJobsBuffer {
    fn new() -> Self {
        CompletedJobsBuffer {
            jobs: Vec::with_capacity(COMPLETED_JOBS_CAPACITY),
            index: 0,
            is_full: false,
        }
    }

    /// Add a completed job to the circular buffer
    fn add(&mut self, job_id: String) {
        if self.jobs.len() < COMPLETED_JOBS_CAPACITY {
            self.jobs.push(job_id);
        } else {
            self.is_full = true;
            self.jobs[self.index] = job_id;
            self.index = (self.index + 1) % COMPLETED_JOBS_CAPACITY;
        }
    }

    /// Check if a job ID is in the completed jobs list
    fn contains(&self, job_id: &str) -> bool {
        self.jobs.iter().any(|id| id == job_id)
    }
}

/// Manages the persistent WebSocket connection to the backend
pub struct WebSocketManager {
    backend_url: String,
    backend_port: String,
    client_id: String,
    completed_jobs: Mutex<CompletedJobsBuffer>,
}

impl WebSocketManager {
    /// Creates a new WebSocket manager and starts the connection
    pub async fn new(
        backend_url: String,
        backend_port: String,
        client_id: String,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let ws_url = format!(
            "ws://{}:{}/ws?clientId={}",
            backend_url, backend_port, client_id
        );

        debug!("🔌 Connecting to backend WebSocket at: {}", ws_url);

        let (ws_stream, _) = connect_async(&ws_url).await?;
        debug!("✅ Connected to backend WebSocket");

        let (_write, read) = ws_stream.split();

        let manager = Arc::new(WebSocketManager {
            backend_url,
            backend_port,
            client_id,
            completed_jobs: Mutex::new(CompletedJobsBuffer::new()),
        });

        // Spawn the message listener task
        let manager_clone = Arc::clone(&manager);
        tokio::spawn(WebSocketManager::message_listener(read, manager_clone));

        Ok(manager)
    }

    /// The main message listener loop
    async fn message_listener(
        mut read: SplitStream<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>,
        manager: Arc<WebSocketManager>,
    ) {
        loop {
            match read.next().await {
                Some(Ok(msg)) => {
                    if let Message::Text(text) = msg {
                        debug!("📨 WebSocket message received: {}", text);

                        // Parse the message
                        if let Ok(json) = serde_json::from_str::<Value>(&text) {
                            if let Some(msg_type) = json.get("type").and_then(|v| v.as_str()) {
                                if msg_type == "executing" {
                                    if let Some(data) = json.get("data").and_then(|v| v.as_object())
                                    {
                                        if let Some(prompt_id) =
                                            data.get("prompt_id").and_then(|v| v.as_str())
                                        {
                                            // Check if node is null (job completion)
                                            if let Some(node) = data.get("node") {
                                                if node.is_null() {
                                                    debug!(
                                                        "✅ Job completed for prompt_id: {}",
                                                        prompt_id
                                                    );
                                                    // Add to completed jobs buffer
                                                    let mut jobs = manager.completed_jobs.lock().await;
                                                    jobs.add(prompt_id.to_string());
                                                } else if let Some(node_str) = node.as_str() {
                                                    debug!(
                                                        "🔄 Job {} executing node: {}",
                                                        prompt_id, node_str
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Some(Err(e)) => {
                    error!("❌ WebSocket error: {}", e);
                    break;
                }
                None => {
                    error!("❌ WebSocket connection closed");
                    break;
                }
            }
        }
    }

    /// Waits for a job to complete by monitoring WebSocket messages
    pub async fn wait_for_job_completion(&self, prompt_id: &str) -> Result<(), Box<dyn std::error::Error>> {
        
        debug!("⏳ Waiting for job completion for prompt_id: {}", prompt_id);

        loop {
            {
                let jobs = self.completed_jobs.lock().await;
                if jobs.contains(prompt_id) {
                    debug!("⚡ Job {} already completed (found in cache)", prompt_id);
                    return Ok(());
                }
            }
            // sleep
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}
