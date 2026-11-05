use crate::{ErrorKind, ModelError};
use serde_json::Value;

pub(super) fn classified(body: &Value, status: Option<u16>) -> ModelError {
    let error = body
        .get("error")
        .filter(|error| error.is_object())
        .unwrap_or(body);
    let code = error["code"].as_str().unwrap_or_default();
    let (kind, code_status) = match code {
        "subscription_sharing_usage_limit_exceeded" => (ErrorKind::ChatgptUsageLimit, Some(429)),
        "subscription_sharing_user_not_eligible" => (ErrorKind::ChatgptNotEligible, Some(403)),
        "subscription_sharing_unsupported_capability" => {
            (ErrorKind::ChatgptUnsupportedCapability, Some(400))
        }
        "subscription_sharing_route_not_supported" => (ErrorKind::ChatgptRoute, Some(403)),
        "subscription_sharing_invalid_user" => (ErrorKind::ChatgptAuthorization, Some(401)),
        "chatpass_v2_scope_not_authorized" | "chatpass_v2_invalid_authorization_context" => {
            (ErrorKind::ChatgptPermission, Some(403))
        }
        "invalid_encrypted_content" => (ErrorKind::ChatgptEncryptedContent, Some(400)),
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => {
            (ErrorKind::Server, Some(503))
        }
        "context_length_exceeded" | "context_window_exceeded" => {
            (ErrorKind::ContextLength, Some(400))
        }
        "rate_limit_exceeded" => (ErrorKind::RateLimited, Some(429)),
        "server_error" => (ErrorKind::Server, Some(500)),
        _ => (
            match status {
                Some(401) => ErrorKind::ChatgptAuthorization,
                Some(403) => ErrorKind::ChatgptPermission,
                Some(400 | 422) => ErrorKind::InvalidRequest,
                Some(408) => ErrorKind::Timeout,
                Some(429) => ErrorKind::RateLimited,
                Some(500..=599) => ErrorKind::Server,
                _ => ErrorKind::Other,
            },
            None,
        ),
    };
    ModelError {
        kind,
        status: status.or(code_status),
        retry_after: None,
        retries: 0,
    }
}

pub(super) async fn http(mut response: reqwest::Response) -> ModelError {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(crate::retry::parse_retry_after);
    // Error bodies can contain credentials echoed by the remote server.
    // Bound inspection, classify known codes, and discard every remote string.
    let mut bytes = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if bytes.len() + chunk.len() > 65_536 {
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let mut error = classified(&body, Some(status));
    error.retry_after = retry_after;
    error
}

pub(super) fn transport(error: reqwest::Error) -> ModelError {
    ModelError::new(if error.is_timeout() {
        ErrorKind::Timeout
    } else if error.is_builder() {
        ErrorKind::InvalidRequest
    } else {
        ErrorKind::Network
    })
}
