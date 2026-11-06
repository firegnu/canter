use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct DoctorParams {
    pub client_version: String,
    pub client_protocol_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum DoctorCategory {
    Configuration,
    Version,
    Authentication,
    Provider,
    Mcp,
    Sandbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum DoctorStatus {
    Ok,
    Fail,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheck {
    pub id: String,
    pub name: String,
    pub category: DoctorCategory,
    pub status: DoctorStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct DoctorResponse {
    pub data: Vec<DoctorCheck>,
    pub server_version: String,
    pub server_protocol_version: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes;

    #[test]
    fn doctor_response_uses_stable_public_codes_without_credentials() {
        let response = DoctorResponse {
            data: vec![DoctorCheck {
                id: "provider.example".into(),
                name: "Provider example".into(),
                category: DoctorCategory::Provider,
                status: DoctorStatus::Fail,
                reason: Some("Provider example rejected its API key (HTTP 401).".into()),
                fix: Some("Replace api_key for provider example in config.json.".into()),
            }],
            server_version: "0.1.0 · test".into(),
            server_protocol_version: 1,
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["data"][0]["category"], "provider");
        assert_eq!(value["data"][0]["status"], "fail");
        assert_eq!(value["serverProtocolVersion"], 1);
        let decoded: DoctorResponse = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, response);
        assert_eq!(routes::DOCTOR, "/v1/doctor");
    }
}
