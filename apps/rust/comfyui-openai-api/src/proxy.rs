//! HTTP proxy handler and routing logic
//!
//! This module implements the core proxy functionality, routing incoming OpenAI API
//! requests to ComfyUI backend and handling error responses.

use axum::{
    body::{Body},
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response as AxumResponse},
};
use log::{error};
use reqwest::Client;
use serde::Serialize;
use std::{collections::HashMap, sync::Arc, time::Duration};
use crate::ws::WebSocketManager;
use crate::comfyui::{generations_response};
use serde_json::Value;

/// Shared state passed to all request handlers
///
/// In web servers, each incoming request is handled by a separate async task.
/// The ProxyState struct allows you to share the HTTP Client and other resources
/// across all these concurrent request handlers without creating duplicates.
/// This is essential for efficient resource usage and connection pooling.
#[derive(Clone)]
pub struct ProxyState {
    /// Reusable HTTP client for ComfyUI backend communication
    pub client: Client,
    /// ComfyUI backend host address
    pub backend_url: String,
    /// ComfyUI backend port number
    pub backend_port: String,
    /// Unique client ID for ComfyUI WebSocket connections
    pub backend_client_id: String,
    /// Maximum allowed request body size in megabytes
    pub max_payload_size_mb: usize,
    /// Request timeout in seconds
    pub timeout: u64,
    /// WebSocket manager for job completion tracking
    pub ws_manager: Arc<WebSocketManager>,
    /// Whether to use or not WebSockets
    pub use_ws: bool,
    /// Pre-loaded workflows (workflows) keyed by model name
    pub workflows: Arc<HashMap<String, Value>>,
}

/// Error types for proxy operations
///
/// Categorizes different error scenarios to enable appropriate HTTP status codes
/// and error messages in responses
#[derive(Debug)]
pub enum ProxyError {
    /// Internal server errors (500)
    Internal(String),
    /// Upstream/backend errors (502)
    Upstream(String),
    /// JSON parsing/serialization errors (400)
    Json(String),
    /// Request validation errors (400)
    #[allow(dead_code)]
    Validation(String),
    /// Unimplemented features (400)
    Implementation(String),
}

/// HTTP response body for error messages
#[derive(Serialize)]
struct ErrorResponse {
    /// HTTP status code
    code: u32,
    /// Error message
    message: String,
}

/// Converts ProxyError into an HTTP response with appropriate status code and JSON body
impl IntoResponse for ProxyError {
    fn into_response(self) -> AxumResponse {
        let (status, _code, message) = match self {
            ProxyError::Internal(msg) => {
                error!("❌ Internal error: {}", msg);
                (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", msg)
            }
            ProxyError::Upstream(msg) => {
                error!("❌ Upstream error: {}", msg);
                (StatusCode::BAD_GATEWAY, "UPSTREAM_ERROR", msg)
            }
            ProxyError::Json(msg) => {
                error!("❌ JSON error: {}", msg);
                (StatusCode::BAD_REQUEST, "JSON_ERROR", msg)
            }
            ProxyError::Validation(msg) => {
                error!("❌ Validation error: {}", msg);
                (StatusCode::BAD_REQUEST, "VALIDATION_ERROR", msg)
            }
            ProxyError::Implementation(msg) => {
                error!("❌ Implementation error: {}", msg);
                (StatusCode::BAD_REQUEST, "IMPLEMENTATION_ERROR", msg)
            }

        };

        let error_response = ErrorResponse {
            code: status.as_u16() as u32,
            message,
        };

        (status, axum::Json(error_response)).into_response()
    }
}

/// Convert reqwest error to ProxyError
impl From<reqwest::Error> for ProxyError {
    fn from(err: reqwest::Error) -> Self {
        ProxyError::Upstream(format!("Request error: {}", err))
    }
}

/// Convert JSON serialization error to ProxyError
impl From<serde_json::Error> for ProxyError {
    fn from(err: serde_json::Error) -> Self {
        ProxyError::Json(format!("JSON error: {}", err))
    }
}

/// Handles request errors from the HTTP client
///
/// Categorizes the error type (timeout, connection, etc.) and returns an appropriate
/// ProxyError with a user-friendly message.
pub fn handle_request_error(e: reqwest::Error, full_url: &str) -> ProxyError {
    error!("❌ Request failed after send(): {:?}", e);
    error!("❌ Is timeout: {}", e.is_timeout());
    error!("❌ Is connect: {}", e.is_connect());
    error!("❌ Is request: {}", e.is_request());
    error!("❌ Is decode: {}", e.is_decode());
    if e.is_timeout() {
        ProxyError::Upstream(format!(
            "Request timeout to {}: {}",
            full_url, e
        ))
    } else if e.is_connect() {
        ProxyError::Upstream(format!(
            "Connection failed to {}: {} - Check if backend server is running",
            full_url, e
        ))
    } else if e.is_request() {
        ProxyError::Upstream(format!(
            "Request error to {}: {}",
            full_url, e
        ))
    } else {
        ProxyError::Upstream(format!(
            "Network error to {}: {}",
            full_url, e
        ))
    }
}

/// Handles timeout errors for requests that exceed the configured timeout
pub fn handle_timeout_error(full_url: &str, timeout_duration: Duration) -> ProxyError {
    error!(
        "❌ Request timed out after {} seconds",
        timeout_duration.as_secs()
    );
    ProxyError::Upstream(format!(
        "Request hung/timed out after {} seconds to {}",
        timeout_duration.as_secs(),
        full_url
    ))
}

/// Main request handler for all /v1/images/* paths
///
/// Routes incoming OpenAI API requests to the appropriate backend handler
/// based on the path segment (e.g., "generations" for /v1/images/generations).
///
/// # Arguments
/// * `state` - Shared proxy state (client, config, workflows)
/// * `path` - The path segment after /v1/images/ (e.g., "generations")
/// * `params` - URL query parameters
/// * `_method` - HTTP method (POST, GET, etc.)
/// * `headers` - HTTP request headers
/// * `body` - Request body
///
/// # Returns
/// - HTTP response for the requested operation
/// - ProxyError if the path is not implemented or other error occurs
pub async fn proxy_handler(
    State(state): State<Arc<ProxyState>>,
    Path(path): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    _method: Method,
    headers: HeaderMap,
    body: Body,
) -> Result<AxumResponse, ProxyError> {

    // Route based on the requested endpoint
    match path.as_str()  {
        // Handle OpenAI image generation API equivalent
        "generations" => {
            return generations_response(State(state), Query(params), headers, body).await;
        }
        // Reject unsupported endpoints
        _ => {
            error!("❌ Path not supported: {}", path);
            return Err(ProxyError::Implementation(format!(
                "Path not supported: {}",
                path
            )));
        }
    }
}
