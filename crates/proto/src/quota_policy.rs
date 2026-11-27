//! Provider usage windows and project quota modes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderQuotaPolicy {
    #[default]
    BalancedFailover,
    PinnedFreeze,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageWindowScope {
    #[default]
    AccountWide,
    ModelFamily(String),
}

/// Extends the account UI's flat usage-window shape with a bucket scope.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScopedUsageWindow {
    pub label: String,
    pub used_fraction: f32,
    pub resets_at: Option<DateTime<Utc>>,
    pub scope: UsageWindowScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_budget_fraction: Option<f32>,
}
