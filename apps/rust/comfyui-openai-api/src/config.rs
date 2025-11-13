use serde::{Deserialize, Serialize};
use std::env;
use std::fs;

#[derive(Debug, Deserialize, Serialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ComfyUiProxyConfig {
    pub host: String,
    pub port: u16,
    pub client_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RoutingConfig {
    pub timeout_seconds: u16,
    pub max_payload_size_mb: u16,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub log_level: String,
    pub server: ServerConfig,
    pub comfyui_backend: ComfyUiProxyConfig,
    pub routing: RoutingConfig,
}

impl Config {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let config_path =
            env::var("CONFIG_PATH").unwrap_or_else(|_| "./config/config.yaml".to_string());

        let config_str = fs::read_to_string(&config_path)
            .map_err(|e| format!("Failed to read config file '{}': {}", config_path, e))?;

        let config: Config = serde_yaml::from_str(&config_str)?;
        Ok(config)
    }
}
