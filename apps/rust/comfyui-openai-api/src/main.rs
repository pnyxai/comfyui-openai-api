//! ComfyUI OpenAI API Proxy Server
//!
//! This application serves as a reverse proxy that translates OpenAI image generation API
//! calls into ComfyUI backend requests. It allows clients to use the OpenAI API standard
//! (v1/images/generations) while leveraging ComfyUI workflows on the backend.
//!
//! The proxy handles:
//! - Configuration loading from YAML files
//! - HTTP to ComfyUI request translation
//! - WebSocket communication for job tracking
//! - Workflow loading and management
//! - Response formatting to match OpenAI standards
//! - Base64 image encoding for API responses

mod config;
mod proxy;
mod comfyui;
mod ws;

use axum::{routing::any, Router};
use log::info;
use reqwest::Client;
use std::{sync::Arc, time::Duration};
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer};

use config::Config;
use proxy::ProxyState;
use ws::WebSocketManager;
use comfyui::WorkflowsLoader;

/// Entry point for the application with 8 worker threads for handling concurrent requests
#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() {
    // Load configuration from YAML file (defaults to ./config/config.yaml or CONFIG_PATH env var)
    let config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Failed to load config: {}", e);
            std::process::exit(1);
        }
    };

    // Configure logging level from config file (debug, info, warn, error)
    unsafe {
        std::env::set_var("RUST_LOG", &config.log_level);
    }
    // Initialize the logger
    env_logger::init();

    // Display loaded configuration for debugging and monitoring
    info!("═══════════════════════════════════════════════════════════");
    info!("📋 Configuration Loaded Successfully");
    info!("═══════════════════════════════════════════════════════════");
    info!("🔧 Server Configuration:");
    info!("   Host: {}", config.server.host);
    info!("   Port: {}", config.server.port);
    info!("");
    info!("🔗 ComfyUI Backend Configuration:");
    info!("   Host: {}", config.comfyui_backend.host);
    info!("   Port: {}", config.comfyui_backend.port);
    info!("   ClientID: {}", config.comfyui_backend.client_id);
    info!("   Workflows Path: {}", config.comfyui_backend.workflows_folder);
    info!("   Use WebSockets: {}", config.comfyui_backend.use_ws);
    info!("");
    info!("⏱️  Routing Configuration:");
    info!("   Timeout (seconds): {}", config.routing.timeout_seconds);
    info!("   Max Payload Size (MB): {}", config.routing.max_payload_size_mb);
    info!("");
    info!("📊 Logger Configuration:");
    info!("   Log Level: {}", config.log_level);
    info!("═══════════════════════════════════════════════════════════");
    info!("");

    // Start the async server runtime with all configuration parameters
    run_server(
        config.server.host,
        config.server.port.to_string(),
        config.comfyui_backend.host,
        config.comfyui_backend.port.to_string(),
        config.comfyui_backend.client_id,
        config.routing.timeout_seconds.into(),
        config.routing.max_payload_size_mb.into(),
        config.comfyui_backend.workflows_folder.into(),
        config.comfyui_backend.use_ws.into(),
    )
    .await;
}

/// Initializes and runs the async web server with all necessary components
///
/// # Arguments
/// * `server_addr` - Proxy server bind address (e.g., "127.0.0.1")
/// * `server_port` - Proxy server bind port (e.g., "8080")
/// * `backend_addr` - ComfyUI backend address (e.g., "localhost")
/// * `backend_port` - ComfyUI backend port (e.g., "8188")
/// * `backend_client_id` - Unique client identifier for ComfyUI WebSocket
/// * `tcp_timeout` - Request timeout in seconds
/// * `max_payload_size_mb` - Maximum request body size in MB
/// * `workflows_config` - Path to directory containing workflows JSON files
async fn run_server(
    server_addr: String,
    server_port: String,
    backend_addr: String,
    backend_port: String,
    backend_client_id: String,
    tcp_timeout: u64,
    max_payload_size_mb: usize,
    workflows_config: String,
    use_ws: bool,
) {
    // Create an optimized HTTP/1.1 client for communicating with ComfyUI backend
    // Configuration prioritizes:
    // - Request timeout for preventing hanging connections
    // - Connection timeout for fast failure detection
    // - TCP nodelay for reduced latency in image transfer
    let client = Client::builder()
        // Maximum time to wait for a complete request/response
        .timeout(Duration::from_secs(tcp_timeout))
        // Maximum time to establish a TCP connection
        .connect_timeout(Duration::from_secs(5))
        // Use HTTP/1.1 only (HTTP/2 not needed for this use case)
        .http1_only()
        // Disables Nagle's algorithm to reduce latency for large image data
        .tcp_nodelay(true)
        // Bypass system proxy settings for direct backend communication
        .no_proxy()
        // Build the client
        .build()
        // Panic on failure as client creation is critical to server startup
        .expect("Failed to create HTTP client");

    // Initialize WebSocket manager for persistent connection to ComfyUI backend
    // This manages the ws://backend:port/ws?clientId=X connection for job completion tracking
    // TODO : Make tis conditional to `use_ws`
    let ws_manager = match WebSocketManager::new(
        backend_addr.clone(),
        backend_port.clone(),
        backend_client_id.clone(),
    )
    .await
    {
        Ok(manager) => {
            info!("✅ WebSocket manager initialized successfully");
            manager
        }
        Err(e) => {
            eprintln!("Failed to initialize WebSocket manager: {}", e);
            std::process::exit(1);
        }
    };

    // Load all workflow JSON files from the specified directory
    // Workflows are ComfyUI workflow definitions that map to specific models
    let workflows = match WorkflowsLoader::load_from_folder(&workflows_config.to_string()) {
                Ok(workflows_map) => {
                    info!("✅ Workflows loaded successfully");
                    Arc::new(workflows_map)
                }
                Err(e) => {
                    eprintln!("Failed to load workflows: {}", e);
                    std::process::exit(1);
                }
            };

    // Wrap shared state in Arc (atomic reference counter) to safely share across
    // concurrent async tasks. This is required for the async handler functions
    // to access the HTTP client, WebSocket manager, and workflows concurrently.
    let porxy_state = Arc::new(ProxyState {
        client,
        backend_url: backend_addr,
        backend_port: backend_port,
        backend_client_id: backend_client_id,
        max_payload_size_mb: max_payload_size_mb,
        timeout: tcp_timeout + 10, // Add 10 second buffer to timeout
        use_ws: use_ws,
        ws_manager,
        workflows,
    });

    // Configure the Axum web server with routing and middleware
    let app = Router::new()
        // Route all HTTP methods to /v1/images/* path to match OpenAI API standard
        // This captures paths like /v1/images/generations
        .route("/v1/images/*path", any(proxy::proxy_handler))
        // Enable CORS to allow requests from any origin
        .layer(CorsLayer::permissive())
        // Set request body size limit to prevent memory exhaustion
        .layer(RequestBodyLimitLayer::new(
            max_payload_size_mb * 1024 * 1024,
        ))
        // Make shared state (client, ws_manager, workflows) available to all handlers
        .with_state(porxy_state);

    // Bind the proxy server to the specified address and port
    let listener = tokio::net::TcpListener::bind(format!("{}:{}", server_addr, server_port))
        .await
        .expect("Failed to bind to port");

    // Log successful binding
    if let Ok(socket) = listener.local_addr() {
        info!("High-performance proxy server running on {}", socket);
        info!("Worker threads: 8 (configured)");
        info!("Ready to handle high traffic loads...");
    }

    // Start the Axum server with graceful shutdown handling
    axum::serve(
        listener,
        // Convert the router into a service that can access client connection info
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    // Enable graceful shutdown on Ctrl+C or SIGTERM
    .with_graceful_shutdown(shutdown_signal())
    // Run the server until shutdown is triggered
    .await
    .expect("Server failed");
}

/// Sets up signal handlers for graceful server shutdown
///
/// Listens for Ctrl+C (SIGINT) or SIGTERM and initiates graceful shutdown
/// to allow in-flight requests to complete before stopping.
async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    // Wait for either Ctrl+C or SIGTERM
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Gracefully shutting down proxy server...");
}
