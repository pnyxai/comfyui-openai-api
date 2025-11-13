use axum::{
    body::{Body},
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response as AxumResponse},
};
use log::{debug, error, warn};
use reqwest::Client;
use serde::Serialize;
use std::{collections::HashMap, sync::Arc, time::Duration};
use crate::ws::WebSocketManager;
use crate::comfyui::{generations_response};



// In web servers, each incoming request is handled by a separate async task.
// The ProxyState struct allows you to share the HTTP Client across all these
//  concurrent request handlers without creating a new client for each request.
#[derive(Clone)]
pub struct ProxyState {
    pub client: Client,
    pub backend_url: String,
    pub backend_port: String,
    pub backend_client_id: String,
    pub max_payload_size_mb: usize,
    pub timeout: u64,
    pub ws_manager: Arc<WebSocketManager>,
}

#[derive(Debug)]
pub enum ProxyError {
    Internal(String),
    Upstream(String),
    Json(String),
    #[allow(dead_code)]
    Validation(String),
    Implementation(String),
}

#[derive(Serialize)]
struct ErrorResponse {
    code: u32,
    message: String,
}

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

impl From<reqwest::Error> for ProxyError {
    fn from(err: reqwest::Error) -> Self {
        ProxyError::Upstream(format!("Request error: {}", err))
    }
}

impl From<serde_json::Error> for ProxyError {
    fn from(err: serde_json::Error) -> Self {
        ProxyError::Json(format!("JSON error: {}", err))
    }
}

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


pub fn handle_timeout_error(full_url: &str, timeout_duration: Duration) -> ProxyError {
    error!(
        "❌ Request timed out after {} seconds",
        timeout_duration.as_secs()
    );
    return ProxyError::Upstream(format!(
        "Request hung/timed out after {} seconds to {}",
        timeout_duration.as_secs(),
        full_url
    ));
}

pub async fn proxy_handler(
    State(state): State<Arc<ProxyState>>,
    Path(path): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    _method: Method,
    headers: HeaderMap,
    body: Body,
) -> Result<AxumResponse, ProxyError> {

    // Check if the requested path is implemented
    match path.as_str()  {
        "generations" => {
            // Call the backend
            return generations_response(State(state), Query(params), headers, body).await;
        }
        _ => {
            error!("❌ Path not supported: {}", path);
            return Err(ProxyError::Implementation(format!(
                "Path not supported: {}",
                path
            )));
        }
        
    }
}
