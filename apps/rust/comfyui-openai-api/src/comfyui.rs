use axum::{
    body::{Body, Bytes},
    extract::{Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method},
    response::{Response as AxumResponse},
};
use log::{debug, error, warn};
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use std::{collections::HashMap, str::FromStr, sync::Arc, time::Duration};
use crate::ws::WebSocketManager;
use base64::{engine::general_purpose, Engine as _};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
struct ImageFile {
    filename: String,
    subfolder: String,
    #[serde(rename = "type")]
    type_field: String,
}

use crate::proxy::{ProxyState, ProxyError, handle_request_error, handle_timeout_error};


pub async fn generations_response(
    State(state): State<Arc<ProxyState>>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Body,
) -> Result<AxumResponse, ProxyError> {

    // Set the backend target endpoint
    let target_base: String = format!("{}:{}", state.backend_url, state.backend_port);
    let method = Method::POST;
    // Append the target path
    let target_url: String = format!("http://{}/prompt", target_base);

    debug!("🎯 Proxying {} / -> {}", method, target_url);

    // Build query string, if any, after the backend path (not really tested)
    let query_string = if params.is_empty() {
        String::new()
    } else {
        let mut query = String::with_capacity(256);
        query.push('?');
        for (i, (k, v)) in params.iter().enumerate() {
            if i > 0 {
                query.push('&');
            }
            query.push_str(k);
            query.push('=');
            query.push_str(v);
        }
        query
    };
    let full_url = format!("{}{}", target_url, query_string);

    // Read body of the request, up to the given number of MBs
    debug!("📥 Reading request body...");
    let body_bytes = match axum::body::to_bytes(body, state.max_payload_size_mb * 1024 * 1024).await
    {
        Ok(bytes) => {
            debug!("✅ Body read successfully: {} bytes", bytes.len());
            bytes
        }
        Err(e) => {
            error!("❌ Failed to read body: {}", e);
            return Err(ProxyError::Internal(format!(
                "Failed to read request body: {}",
                e
            )));
        }
    };

    // There is some body here, construct the comfyui request
    let processed_body = if !body_bytes.is_empty() {
        debug!("🔧 Generating comfyui request body...");
        match create_json_payload(
            body_bytes,
            state.backend_client_id.clone(),
        )
        .await
        {
            Ok(modified) => {
                debug!("✅ Body modified successfully");
                modified
            }
            Err(e) => {
                warn!("❌ Failed to modify body: {:?}", e);
                return Err(e);
            }
        }
    } else {
        body_bytes
    };

    // Prepare headers
    debug!("📋 Preparing headers...");
    let mut upstream_headers = reqwest::header::HeaderMap::new();

    // Only add content-type if we have a body
    if !processed_body.is_empty() {
        upstream_headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
    }

    // Add authorization header if present in original request
    if let Some(auth) = headers.get("authorization") {
        if let Ok(auth_value) = reqwest::header::HeaderValue::from_bytes(auth.as_bytes()) {
            upstream_headers.insert(reqwest::header::AUTHORIZATION, auth_value);
        }
    }

    // Debug: Print all headers being sent
    debug!("📋 Headers to send:");
    for (name, value) in upstream_headers.iter() {
        debug!("   {}: {}", name, value.to_str().unwrap_or("[unprintable]"));
    }
    debug!("🚀 Making upstream request...");
    debug!("   URL: {}", full_url);
    debug!("   Method: {}", method);
    debug!("   Body size: {} bytes", processed_body.len());

    // Build request
    let request_builder = state
        // set client
        .client
        // add method and endpoint
        .request(method.clone(), &full_url)
        // add headers
        .headers(upstream_headers)
        // add body
        .body(processed_body);

    debug!("⏳ Sending request to backend...");
    debug!("🔍 About to call request_builder.send()...");

    // Add a timeout wrapper to catch hanging requests
    let request_future = request_builder.send();
    let timeout_duration = Duration::from_secs(state.timeout);

    debug!(
        "⏰ Starting request with {} second timeout...",
        timeout_duration.as_secs()
    );

    // Await the request future
    let upstream_response = match tokio::time::timeout(timeout_duration, request_future).await {
        Ok(Ok(response)) => {
            debug!(
                "✅ Got response from backend: {} - Headers: {:?}",
                response.status(),
                response.headers()
            );
            response
        }
        Ok(Err(e)) => {
            return Err(handle_request_error(e, &full_url));
        }
        Err(_) => {
            return Err(handle_timeout_error(&full_url, timeout_duration));
        }
    };

    // Check if it is a streaming request
    let is_streaming = upstream_response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.contains("text/event-stream")
                || v.contains("application/x-ndjson")
                || v.contains("text/plain")
        })
        .unwrap_or(false);

    println!(
        "📦 Response type: {}",
        if is_streaming { "streaming" } else { "regular" }
    );


    // Handle a regular response
    handle_regular_response(
        upstream_response,
        target_base, 
        headers,
        &state.client,
        &state.ws_manager,
    )
    .await
}



/// Modifies the json payload of a openAI image request to adapt it to ComfyUI
async fn create_json_payload(
    body: Bytes,
    client_id: String,
) -> Result<Bytes, ProxyError> {
    // just in case empty body check
    if body.is_empty() {
        return Ok(body);
    }

    // Read all the body as a json, it should be a json
    let mut json: Value = serde_json::from_slice(&body)
        .map_err(|e| ProxyError::Json(format!("Failed to parse JSON: {}", e)))?;

    // // if we can get a json hashmap, go ahead
    // if let Some(openai_request) = json.as_object() {

    //     if let Some(model_name) = openai_request.get("model").and_then(|v| v.as_string()) {
    //         // 

    //     } else {
    //         Err(ProxyError::Json(format!("Failed to get model name from JSON")))
    //     }



    //     if obj.contains_key("client_id") {
    //         obj.insert(
    //             "client_id".to_string(),
    //             Value::String(client_id.clone()),
    //         );
    //     }

    //     debug!("🔧 Modified JSON payload");

    // }

    let modified_json = serde_json::to_vec(&json)
        .map_err(|e| ProxyError::Json(format!("Failed to serialize JSON: {}", e)))?;

    Ok(Bytes::from(modified_json))
}

/// Regular response handler, will receive the response and then send it back to
/// the proxied source
async fn handle_regular_response(
    upstream_response: reqwest::Response,
    target_base: String, 
    headers: HeaderMap,
    client: &Client,
    ws_manager: &Arc<WebSocketManager>,
) -> Result<AxumResponse, ProxyError> {
    let status = upstream_response.status();
    let headers = upstream_response.headers().clone();

    debug!("📄 Handling regular response with status: {}", status);

    let body_bytes = upstream_response
        .bytes()
        .await
        .map_err(|e| ProxyError::Upstream(format!("Failed to read response body: {}", e)))?;

    debug!("📥 Response body: {} bytes", body_bytes.len());

    // Read all the body as a json, it should be a json
    let json: Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| ProxyError::Json(format!("Failed to parse JSON: {}", e)))?;

    debug!(
            "📝 Received response: {}",
            json.to_string()
        );

    // Check for prompt_id in the response
    let prompt_id = json.get("prompt_id").and_then(|v| v.as_str());
    if let Some(pid) = prompt_id {
        debug!(
            "📝 Found prompt_id in response: {}. Waiting for job completion...",
            pid
        );

        // Wait for the job to complete via WebSocket
        if let Err(e) = ws_manager.wait_for_job_completion(pid).await {
            warn!("⚠️ Failed to wait for job completion: {}", e);
            // Continue anyway, don't fail the response
        }
    }

    // Call history to retrieve image location
    let image_response_json = retrieve_image_from_history(
        target_base,
        prompt_id,
        headers.clone(),
        client,
    )
    .await?;



    let output_json = serde_json::to_vec(&image_response_json)
        .map_err(|e| ProxyError::Json(format!("Failed to serialize JSON: {}", e)))?;
    let output_body_bytes = Bytes::from(output_json);
    debug!(
        "✏️ JSON regular response: {} bytes",
        output_body_bytes.len()
    );

    // Copy headers
    let mut response_headers = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers.iter() {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_str(name.as_str()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            if name.as_str() == "content-length" {
                // Replace context len with valid value
                if let Ok(value) =
                    HeaderValue::from_str(format!("{}", output_body_bytes.len()).as_str())
                {
                    response_headers.insert(name, value);
                }
            } else {
                response_headers.insert(name, value);
            }
        }
    }

    let mut response = AxumResponse::builder().status(status.as_u16());

    for (name, value) in response_headers.iter() {
        response = response.header(name, value);
    }

    debug!("✅ Regular response built successfully");

    response
        .body(Body::from(output_body_bytes))
        .map_err(|e| ProxyError::Internal(format!("Failed to build regular response: {}", e)))
}


async fn retrieve_image_from_history(
    target_base: String,
    prompt_id: Option<&str>,
    headers: HeaderMap,
    client: &Client,
) -> Result<Value, ProxyError> {

    // Handle case where prompt_id is None
    let prompt_id = match prompt_id {
        Some(id) => id,
        None => {
            error!("⚠️ No prompt_id received!");
            return Err(ProxyError::Upstream(format!(
                    "No prompt_id received.",
                )));
        }
    };

    // Append the target path
    let history_url: String = format!("http://{}/history/{}", target_base, prompt_id);

    debug!("🔍 Checking history at {} for {}", target_base, prompt_id);

    let mut upstream_headers = reqwest::header::HeaderMap::new();

    // Add authorization header if present in original request
    if let Some(auth) = headers.get("authorization") {
        if let Ok(auth_value) = reqwest::header::HeaderValue::from_bytes(auth.as_bytes()) {
            upstream_headers.insert(reqwest::header::AUTHORIZATION, auth_value);
        }
    }

    // Debug: Print all headers being sent
    debug!("📋 Headers to send (if any):");
    for (name, value) in upstream_headers.iter() {
        debug!("   {}: {}", name, value.to_str().unwrap_or("[unprintable]"));
    }
    
    // Build request
    let request_builder = 
        // set client
        client
        // add method and endpoint
        .request(Method::GET, &history_url)
        // add headers
        .headers(upstream_headers.clone());

    debug!("⏳ Sending history request to backend...");
    
    // Add a timeout wrapper to catch hanging requests
    let request_future = request_builder.send();
    let timeout_duration = Duration::from_secs(5);

    // Await the request future
    let upstream_response = match tokio::time::timeout(timeout_duration, request_future).await {
        Ok(Ok(response)) => {
            debug!(
                "✅ Got response from history backend: {} - Headers: {:?}",
                response.status(),
                response.headers()
            );
            response
        }
        Ok(Err(e)) => {
            return Err(handle_request_error(e, &history_url));
        }
        Err(_) => {
            return Err(handle_timeout_error(&history_url, timeout_duration));
        }
    };


    // Get history data:
    let response_body = upstream_response
        .bytes()
        .await
        .map_err(|e| ProxyError::Upstream(format!("Failed to read history response body: {}", e)))?;
    let history_json: Value = serde_json::from_slice(&response_body)
        .map_err(|e| ProxyError::Json(format!("Failed to parse history JSON: {}", e)))?;
    debug!("✅ Retrieved history: {}", history_json.to_string());


    // Look for job image name and path
    let mut image_files: Vec<ImageFile> = Vec::new();
    if let Some(prompt_hist) = history_json.get(prompt_id).and_then(|v| v.as_object()) {
        if let Some(out_nodes) = prompt_hist.get("outputs").and_then(|v| v.as_object())
        {
            for (node_id, out_node_data) in out_nodes {
                debug!("📨 {}: ", node_id);
                if let Some(all_images_data) = out_node_data.get("images").and_then(|v| v.as_array())
                {
                    for image_data in all_images_data {
                        if let Some(type_field) = image_data["type"].as_str() {
                            if type_field == "output" {
                                if let Some(filename) = image_data["filename"].as_str() {
                                    if let Some(subfolder) = image_data["subfolder"].as_str() {
                                        debug!("Found: filename: {}, subfolder: {}", filename, subfolder);
                                        image_files.push(ImageFile {
                                            filename: filename.to_string(),
                                            subfolder: subfolder.to_string(),
                                            type_field: type_field.to_string()
                                        });
                                    }
                                }
                            }
                            
                        }
                    }
                }
            }
        }
    } else {
        error!("⚠️ No prompt_id history found");
        return Err(ProxyError::Upstream(format!(
                "No prompt_id history found.",
            )));
    }

    debug!("📦 Collected {} image files", image_files.len());
    let mut response_data: Vec<serde_json::Value> = Vec::new();
    for image_file_data in image_files {

        let view_query = serde_urlencoded::to_string(&image_file_data).
            map_err(|e| ProxyError::Json(format!("Failed to serialize image data query: {}", e)))?;

        let view_url: String = format!("http://{}/view?{}", target_base, view_query);


        // Build request
        let request_builder = 
            // set client
            client
            // add method and endpoint
            .request(Method::GET, &view_url)
            // add headers
            .headers(upstream_headers.clone());

        debug!("⏳ Sending view request to backend: {}", view_query);
        
        // Add a timeout wrapper to catch hanging requests
        let request_future = request_builder.send();
        let timeout_duration = Duration::from_secs(5);

        // Await the request future
        let view_response = match tokio::time::timeout(timeout_duration, request_future).await {
            Ok(Ok(response)) => {
                debug!(
                    "✅ Got response from view backend: {} - Headers: {:?}",
                    response.status(),
                    response.headers()
                );
                response
            }
            Ok(Err(e)) => {
                return Err(handle_request_error(e, &view_url));
            }
            Err(_) => {
                return Err(handle_timeout_error(&view_url, timeout_duration));
            }
        };

        
        let image_bytes = view_response.bytes().await?;
        debug!("📋 Read {} image bytes", image_bytes.len());
        let b64_image = general_purpose::STANDARD.encode(image_bytes);
        response_data.push(serde_json::json!({
            "b64_json": b64_image
        }));
    }


    // Create response with image files
    let created = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_err(|_| ProxyError::Internal("Failed to get current time".to_string()))?
    .as_secs() as i64;

     Ok(serde_json::json!({
       "data": response_data,
       "created": created
   }))

}