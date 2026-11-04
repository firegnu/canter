//! Account commands are private HTTP exchanges, never session events.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AccountReadParams {
    pub provider: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    pub id: String,
    pub email: Option<String>,
    pub status: AccountStatus,
    pub plan_authorized: bool,
    pub background_consent: bool,
    pub pending_revocation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum AccountStatus {
    LoggedIn,
    PlanNotAuthorized,
    NeedsLogin,
    SignedOut,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountReadResponse {
    pub provider: String,
    pub accounts: Vec<AccountInfo>,
    pub current_account_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginStartParams {
    pub provider: String,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub background_consent: bool,
}

/// The URL contains ephemeral OAuth state; deliberately not Debug.
#[derive(Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginStartResponse {
    pub provider: String,
    pub login_id: String,
    pub auth_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginParams {
    pub provider: String,
    pub login_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum AccountLoginCancelStatus {
    Canceled,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AccountLoginCancelResponse {
    pub provider: String,
    pub status: AccountLoginCancelStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginCompleted {
    pub provider: String,
    pub login_id: String,
    pub success: bool,
    pub error: Option<String>,
    pub account: Option<AccountInfo>,
    pub show_plan_notice: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AccountSelectParams {
    pub provider: String,
    /// Email, one-based number in account/read, or a local record ID.
    pub account: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AccountBackgroundParams {
    pub provider: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AccountLogoutParams {
    pub provider: String,
    #[serde(default)]
    pub account: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum AccountLogoutResult {
    Revoked,
    Pending,
    NoToken,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AccountLogoutResponse {
    pub provider: String,
    pub account: AccountInfo,
    pub result: AccountLogoutResult,
}
