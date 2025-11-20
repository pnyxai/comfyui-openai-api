//! Configuration management for the ComfyUI OpenAI API proxy
//!
//! This module handles loading and validating configuration from YAML files.
//! Configuration can be customized via the CONFIG_PATH environment variable.

use serde::{Deserialize, Serialize};
use std::env;
use std::fs;

/// Proxy server configuration
/// 
/// Defines where the proxy server listens for incoming OpenAI API requests
#[derive(Debug, Deserialize, Serialize)]
pub struct ServerConfig {
    /// Server bind address (e.g., "0.0.0.0", "127.0.0.1")
    pub host: String,
    /// Server bind port (e.g., 8080)
    pub port: u16,
}

/// ComfyUI backend connection configuration
/// 
/// Defines how to connect to the ComfyUI server and which workflows to use
#[derive(Debug, Deserialize, Serialize)]
pub struct ComfyUiProxyConfig {
    /// ComfyUI backend host address
    pub host: String,
    /// ComfyUI backend port
    pub port: u16,
    /// Unique client identifier for WebSocket connections to ComfyUI
    pub client_id: String,
    /// Path to directory containing workflow JSON files
    pub workflows_folder: String,
    /// Whether to use or not WebSockets
    pub use_ws: bool,
}

/// Request routing and timeout configuration
/// 
/// Controls request handling behavior and limits
#[derive(Debug, Deserialize, Serialize)]
pub struct RoutingConfig {
    /// Timeout in seconds for requests to ComfyUI backend
    pub timeout_seconds: u16,
    /// Maximum request body size in megabytes
    pub max_payload_size_mb: u16,
}

/// Root configuration struct loaded from YAML
/// 
/// Example config.yaml:
/// ```yaml
/// log_level: debug
/// server:
///   host: "0.0.0.0"
///   port: 8080
/// comfyui_backend:
///   host: "localhost"
///   port: 8188
///   client_id: "openai-proxy-client"
///   workflows_folder: "./workflows"
/// routing:
///   timeout_seconds: 120
///   max_payload_size_mb: 10
/// ```
#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    /// Logging level: debug, info, warn, error
    pub log_level: String,
    /// Proxy server configuration
    pub server: ServerConfig,
    /// ComfyUI backend configuration
    pub comfyui_backend: ComfyUiProxyConfig,
    /// Request routing configuration
    pub routing: RoutingConfig,
}

impl Config {
    /// Load configuration from a YAML file
    /// 
    /// Reads from CONFIG_PATH environment variable or defaults to "./config/config.yaml"
    /// 
    /// # Examples
    /// ```no_run
    /// let config = Config::load()?;
    /// println!("Server running on {}:{}", config.server.host, config.server.port);
    /// ```
    /// 
    /// # Errors
    /// Returns an error if:
    /// - The configuration file cannot be read
    /// - The YAML syntax is invalid
    /// - Required fields are missing
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let config_path =
            env::var("CONFIG_PATH").unwrap_or_else(|_| "./config/config.yaml".to_string());

        let config_str = fs::read_to_string(&config_path)
            .map_err(|e| format!("Failed to read config file '{}': {}", config_path, e))?;

        let config: Config = serde_yaml::from_str(&config_str)?;
        Ok(config)
    }
}
