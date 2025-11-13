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

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() {
    // Load config
    let config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Failed to load config: {}", e);
            std::process::exit(1);
        }
    };

    // Set log level from config
    unsafe {
        std::env::set_var("RUST_LOG", &config.log_level);
    }
    // Start the logger
    env_logger::init();

    // Print loaded configuration
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
    info!("");
    info!("⏱️  Routing Configuration:");
    info!("   Timeout (seconds): {}", config.routing.timeout_seconds);
    info!("   Max Payload Size (MB): {}", config.routing.max_payload_size_mb);
    info!("");
    info!("📊 Logger Configuration:");
    info!("   Log Level: {}", config.log_level);
    info!("═══════════════════════════════════════════════════════════");
    info!("");

    // Start the server
    run_server(
        config.server.host,
        config.server.port.to_string(),
        config.comfyui_backend.host,
        config.comfyui_backend.port.to_string(),
        config.comfyui_backend.client_id,
        config.routing.timeout_seconds.into(),
        config.routing.max_payload_size_mb.into(),
    )
    .await;
}

async fn run_server(
    server_addr: String,
    server_port: String,
    backend_addr: String,
    backend_port: String,
    backend_client_id: String,
    tcp_timeout: u64,
    max_payload_size_mb: usize,
) {
    // Simple HTTP1 client
    let client = Client::builder()
        // Maximum time to wait for a complete request/response
        .timeout(Duration::from_secs(tcp_timeout))
        // Maximum time to establish a TCP connection
        .connect_timeout(Duration::from_secs(5))
        // We need no more
        .http1_only()
        // Disables Nagle's algorithm for lower latency
        .tcp_nodelay(true)
        //Bypasses system proxy settings
        .no_proxy()
        // Build it
        .build()
        // Panics if the server fails to start
        .expect("Failed to create HTTP client");

    // Initialize WebSocket manager for backend communication
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

    // Wrap the HTTP client and other needed data in an
    // Arc (atomic reference counter) so it can be safely shared across
    // multiple async tasks
    let porxy_state = Arc::new(ProxyState {
        client,
        backend_url: backend_addr,
        backend_port: backend_port,
        backend_client_id: backend_client_id,
        max_payload_size_mb: max_payload_size_mb,
        timeout: tcp_timeout + 10, // 10 more seconds
        ws_manager,
    });



    // Sets up the web application routing:
    let app = Router::new()
        // Only responds to /v1/images/generations and all HTTP methods
        .route("/v1/images/*path", any(proxy::proxy_handler))
        // Allows cross-origin requests from any domain
        .layer(CorsLayer::permissive())
        // Limits request body size to 10 megabytes
        .layer(RequestBodyLimitLayer::new(
            max_payload_size_mb * 1024 * 1024,
        ))
        // Makes the shared client available to handlers
        .with_state(porxy_state);

    // Bind the server to the provided address
    // let listener = tokio::net::TcpListener::bind(format!("{}:{}", server_addr, server_port))
    let listener = tokio::net::TcpListener::bind(format!("{}:{}", server_addr, server_port))
        .await
        // Panics if the server fails to start
        .expect("Failed to bind to port {}");

    // Log if success
    if let Ok(socket) = listener.local_addr() {
        info!("High-performance proxy server running on {}", socket);
        info!("Worker threads: 8 (configured)");
        info!("Ready to handle high traffic loads...");
    }

    // Start the server
    axum::serve(
        listener,
        // Converts the app into a service that can access client connection info
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    // Sets up signal handling for clean shutdown (Ctrl+C, etc.)
    .with_graceful_shutdown(shutdown_signal())
    // Runs the server until shutdown
    .await
    // Panics if the server fails to start
    .expect("Server failed");
}

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

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Gracefully shutting down proxy server...");
}
