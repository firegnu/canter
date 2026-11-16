//! Ask the server to collect the read-only review input without starting a turn.
use crate::client::Client;
use canter_protocol::SessionInputParams;

pub async fn prepare(
    client: &Client,
    session: &str,
    target: &str,
    client_name: &str,
) -> Result<SessionInputParams, String> {
    client
        .workspace_review(session, target, client_name)
        .await
        .map_err(|error| match error {
            crate::client::ClientError::Server { message, .. } => message,
            error => error.to_string(),
        })
}
