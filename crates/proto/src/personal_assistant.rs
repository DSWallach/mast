//! The personal assistant chat: one user-designated chat per device whose
//! agent tools may read every other chat, in every Space. The widening is
//! read-only (list, get, read); sends, interrupts and archives keep their
//! normal reach. Only the user sets or clears it (IPC RPCs); no agent tool
//! does.

use serde::{Deserialize, Serialize};

/// The device's personal assistant designation (device-local; never synced).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalAssistant {
    pub chat_id: String,
    pub designated_at_ms: i64,
}

/// Reply of `GetPersonalAssistant`, `SetPersonalAssistant` and
/// `ClearPersonalAssistant`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalAssistantReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant: Option<PersonalAssistant>,
}
