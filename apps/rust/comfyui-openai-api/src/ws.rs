//! WebSocket connection manager for ComfyUI backend communication
//!
//! This module manages a persistent WebSocket connection to the ComfyUI backend
//! to track job execution progress and completion status. Jobs are identified by
//! prompt_id, and the manager uses a circular buffer to cache recently completed jobs.
//!
//! The connection includes automatic reconnection with exponential backoff to handle
//! temporary network issues or hung connections.

use axum::http::HeaderMap;
use log::{debug, error, warn};
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures::stream::{StreamExt, SplitStream};

/// Maximum number of completed job IDs to keep in memory
/// Using a circular buffer prevents unbounded memory growth
const COMPLETED_JOBS_CAPACITY: usize = 100;

/// Reconnection configuration constants
/// Controls retry behavior when WebSocket connection fails
const INITIAL_RECONNECT_DELAY_SECS: u64 = 2;
const MAX_RECONNECT_DELAY_SECS: u64 = 60;
const RECONNECT_BACKOFF_MULTIPLIER: f64 = 1.5;

/// Circular buffer for efficiently tracking recently completed job IDs
///
/// Stores up to COMPLETED_JOBS_CAPACITY job IDs in a fixed-size circular buffer.
/// When full, new entries overwrite the oldest ones. This enables quick checking
/// of whether a job has recently completed without memory leaks.
struct CompletedJobsBuffer {
    /// Fixed-size vector of completed job IDs
    jobs: Vec<String>,
    /// Current write position in the circular buffer
    index: usize,
    /// Flag indicating if the buffer has been filled once
    is_full: bool,
}

impl CompletedJobsBuffer {
    /// Create a new empty circular buffer
    fn new() -> Self {
        CompletedJobsBuffer {
            jobs: Vec::with_capacity(COMPLETED_JOBS_CAPACITY),
            index: 0,
            is_full: false,
        }
    }

    /// Add a completed job ID to the circular buffer
    ///
    /// If the buffer is not yet full, appends to the end.
    /// If full, overwrites the oldest entry and moves the index forward.
    fn add(&mut self, job_id: String) {
        if self.jobs.len() < COMPLETED_JOBS_CAPACITY {
            // Buffer not yet full, just append
            self.jobs.push(job_id);
        } else {
            // Buffer full, overwrite oldest entry
            self.is_full = true;
            self.jobs[self.index] = job_id;
            self.index = (self.index + 1) % COMPLETED_JOBS_CAPACITY;
        }
    }

    /// Check if a job ID is in the completed jobs buffer
    ///
    /// Returns true if the job_id has been recorded in the buffer.
    /// Note: Only recent jobs are tracked (up to COMPLETED_JOBS_CAPACITY).
    fn contains(&self, job_id: &str) -> bool {
        self.jobs.iter().any(|id| id == job_id)
    }
}

/// Manages a persistent WebSocket connection to ComfyUI backend
///
/// Maintains a connection to ws://backend:port/ws?clientId=X for receiving
/// job execution progress and completion messages. Uses a background task
/// to process incoming messages and track job status.
///
/// Includes automatic reconnection with exponential backoff to handle
/// temporary network issues or hung connections.
pub struct WebSocketManager {
    /// Circular buffer of recently completed job IDs
    completed_jobs: Mutex<CompletedJobsBuffer>,
    /// Backend URL for reconnection attempts
    backend_url: String,
    /// Backend port for reconnection attempts
    backend_port: String,
    /// Client ID for reconnection attempts
    client_id: String,
}

impl WebSocketManager {
    /// Creates a new WebSocket manager and establishes connection to ComfyUI backend
    ///
    /// This function:
    /// 1. Connects to ws://backend:port/ws?clientId=clientId
    /// 2. Spawns a background message listener task
    /// 3. Returns the manager wrapped in Arc for thread-safe sharing
    ///
    /// # Arguments
    /// * `backend_url` - ComfyUI backend host
    /// * `backend_port` - ComfyUI backend port
    /// * `client_id` - Unique client identifier for this connection
    ///
    /// # Returns
    /// - WebSocketManager wrapped in Arc on success
    /// - Error if connection fails
    ///
    /// # Panics on Error
    /// The connection is required for job tracking. Exit on failure.
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

        // Establish WebSocket connection
        let (ws_stream, _) = connect_async(&ws_url).await?;
        debug!("✅ Connected to backend WebSocket");

        // Split stream into write and read halves (only use read)
        let (_write, read) = ws_stream.split();

        let manager = Arc::new(WebSocketManager {
            completed_jobs: Mutex::new(CompletedJobsBuffer::new()),
            backend_url: backend_url.clone(),
            backend_port: backend_port.clone(),
            client_id: client_id.clone(),
        });

        // Spawn background task to listen for WebSocket messages
        // This task runs concurrently and updates the completed_jobs buffer
        // It includes reconnection logic to handle dropped or hung connections
        let manager_clone = Arc::clone(&manager);
        tokio::spawn(WebSocketManager::message_listener(read, manager_clone));

        Ok(manager)
    }

    /// Background task that listens for WebSocket messages from ComfyUI
    ///
    /// Processes incoming messages looking for "executing" events with null node values,
    /// which indicate job completion. Updates the completed_jobs buffer when jobs finish.
    ///
    /// ComfyUI WebSocket message format:
    /// ```json
    /// {
    ///   "type": "executing",
    ///   "data": {
    ///     "prompt_id": "...",
    ///     "node": null          // null = job complete, otherwise node ID
    ///   }
    /// }
    /// ```
    ///
    /// This task runs indefinitely and automatically reconnects on connection loss with
    /// exponential backoff to handle temporary network issues or hung connections.
    async fn message_listener(
        mut read: SplitStream<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>,
        manager: Arc<WebSocketManager>,
    ) {
        let mut retry_delay = Duration::from_secs(INITIAL_RECONNECT_DELAY_SECS);
        let mut retry_count = 0u32;

        loop {
            loop {
                debug!("🔄 Waiting for next WebSocket message..."); 
                
                // Use timeout to detect hung connections that never send data
                match tokio::time::timeout(Duration::from_secs(60), read.next()).await {
                    Ok(Some(Ok(msg))) => {
                        // Reset retry state on successful message
                        retry_count = 0;
                        retry_delay = Duration::from_secs(INITIAL_RECONNECT_DELAY_SECS);
                        
                        debug!("📋 Received message type: {:?}", msg); 
                        // Process text messages only
                        if let Message::Text(text) = msg {
                            debug!("📋 Received WS message: {}",text);
                            // Parse JSON message
                            if let Ok(json) = serde_json::from_str::<Value>(&text) {
                                if let Some(msg_type) = json.get("type").and_then(|v| v.as_str()) {
                                    // Look for execution status messages
                                    if msg_type == "executing" {
                                        if let Some(data) = json.get("data").and_then(|v| v.as_object())
                                        {
                                            if let Some(prompt_id) =
                                                data.get("prompt_id").and_then(|v| v.as_str())
                                            {
                                                // Check if node is null (indicates completion)
                                                if let Some(node) = data.get("node") {
                                                    if node.is_null() {
                                                        // Job has completed
                                                        debug!(
                                                            "✅ Job completed for prompt_id: {}",
                                                            prompt_id
                                                        );
                                                        // Add to completed jobs buffer
                                                        let mut jobs = manager.completed_jobs.lock().await;
                                                        jobs.add(prompt_id.to_string());
                                                    } else if let Some(node_num) = node.as_str() {
                                                        // Job is still executing (logging at debug level to avoid spam)
                                                        debug!(
                                                            "🔄 Job {} executing node: {}",
                                                            prompt_id, node_num
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
                    Ok(Some(Err(e))) => {
                        // WebSocket error occurred
                        error!("❌ WebSocket error: {}", e);
                        break; // Exit inner loop to trigger reconnection
                    }
                    Ok(None) => {
                        // WebSocket connection closed by server
                        error!("❌ WebSocket connection closed by server");
                        break; // Exit inner loop to trigger reconnection
                    }
                    Err(_) => { // Timeout
                        debug!("🔄 Recycling WebSocket connection");
                        break; // Exit inner loop to trigger reconnection
                    }
                }
            }
            
            // Reconnection logic: retry with exponential backoff
            loop {
                retry_count += 1;
                warn!(
                    "🔄 Attempting to reconnect to WebSocket (attempt #{}, delay: {}s)",
                    retry_count,
                    retry_delay.as_secs()
                );
                
                // Wait before attempting reconnection
                tokio::time::sleep(retry_delay).await;
                
                // Build WebSocket URL
                let ws_url = format!(
                    "ws://{}:{}/ws?clientId={}",
                    manager.backend_url, manager.backend_port, manager.client_id
                );
                
                // Attempt to reconnect
                match connect_async(&ws_url).await {
                    Ok((ws_stream, _)) => {
                        debug!("✅ Reconnected to backend WebSocket");
                        let (_write, new_read) = ws_stream.split();
                        
                        // Reset retry state on successful reconnection
                        retry_count = 0;
                        retry_delay = Duration::from_secs(INITIAL_RECONNECT_DELAY_SECS);
                        
                        // Continue listening with the new connection
                        read = new_read;
                        break; // Exit reconnection loop, continue main message loop
                    }
                    Err(e) => {
                        error!("❌ Reconnection failed: {}", e);
                        
                        // Calculate next retry delay with exponential backoff
                        let next_delay_secs = 
                            (retry_delay.as_secs_f64() * RECONNECT_BACKOFF_MULTIPLIER)
                                .min(MAX_RECONNECT_DELAY_SECS as f64);
                        retry_delay = Duration::from_secs_f64(next_delay_secs);
                        
                        warn!(
                            "⏱️ Next reconnection attempt in {}s",
                            retry_delay.as_secs()
                        );
                        
                        // Continue looping to retry reconnection
                    }
                }
            }
        }
    }

    /// Waits for a job to complete by polling the completed_jobs buffer
    ///
    /// This function blocks (with 500ms sleep intervals) until the specified job_id
    /// appears in the completed_jobs buffer. Since the background listener task
    /// updates this buffer, this effectively waits for the WebSocket message.
    ///
    /// # Arguments
    /// * `prompt_id` - The job ID to wait for
    ///
    /// # Returns
    /// - Ok(()) when job completion is detected
    /// - Error if something goes wrong (currently never errors)
    ///
    /// # Blocking Behavior
    /// This function runs in an async context but blocks the async task.
    /// It should only be called from async contexts (like within Axum handlers).
    pub async fn wait_for_job_completion(&self, prompt_id: &str) -> Result<(), Box<dyn std::error::Error>> {
        debug!("⏳ Waiting for job completion for prompt_id: {}", prompt_id);

        loop {
            {
                // Check if job has completed (brief lock)
                let jobs = self.completed_jobs.lock().await;
                if jobs.contains(prompt_id) {
                    debug!("⚡ Job {} already completed (found in cache)", prompt_id);
                    return Ok(());
                }
            }
           

            // Sleep briefly to avoid busy waiting
            // Yields control to other async tasks
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}
