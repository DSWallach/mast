//! Durable, predeclared Manager review steps and typed worker completion.
//!
//! These records describe a single-engine workflow. They do not fence a
//! disconnected device that believes it owns the same Space.

use serde::{Deserialize, Serialize};

use crate::{ChatConfig, CreateChatForWorkItemParams};

/// Approved review launch settings, captured before the test worker reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerReviewStep {
    pub step_id: String,
    pub project_id: String,
    pub work_item_id: String,
    pub worker_chat_id: String,
    /// The Manager's original create_chat key for the planned worker.
    pub worker_create_key: String,
    /// Hash of the exact planned create_chat inputs.
    pub worker_inputs_hash: String,
    /// Engine-derived worker launch request to verify at dispatch.
    pub worker_launch: CreateChatForWorkItemParams,
    pub original_manager_chat_id: String,
    /// Stable launch identity, scoped to this Manager role and step.
    pub operation_id: String,
    pub reviewer_config: ChatConfig,
    /// Instructions approved for the reviewer before the worker runs.
    pub review_prompt: String,
}

/// A Manager declares a review before starting its test worker. The engine
/// derives the project, Manager, worker and operation identities from the grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerReviewRegistration {
    pub step_id: String,
    pub work_item_id: String,
    pub worker_create_key: String,
    /// Canonical create_chat inputs; the engine validates and hashes these.
    pub worker_inputs: serde_json::Value,
    pub harness_omitted: bool,
    pub model_omitted: bool,
    pub reviewer_config: ChatConfig,
    pub review_prompt: String,
}

/// Write-once provenance recorded atomically with a reviewer's Chat row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerReviewLaunchMarker {
    pub project_id: String,
    pub step_id: String,
    pub operation_id: String,
    pub inputs_hash: String,
    pub original_manager_chat_id: String,
    pub space_id: String,
    pub cwd: String,
    pub config: Option<ChatConfig>,
}

/// A worker's immutable result for a predeclared review step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerReviewCompletion {
    pub step_id: String,
    pub tested_sha: String,
    pub evidence: String,
}

/// Durable progress. A blocked step retains the reason and responsible owner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ManagerReviewState {
    Pending,
    Accepted {
        completion: ManagerReviewCompletion,
        review_launch: CreateChatForWorkItemParams,
    },
    Blocked {
        /// Absent when the planned test-worker launch failed before completion.
        completion: Option<ManagerReviewCompletion>,
        review_launch: Option<CreateChatForWorkItemParams>,
        reason: String,
        owner: String,
    },
    Started {
        completion: ManagerReviewCompletion,
        review_launch: CreateChatForWorkItemParams,
        reviewer_chat_id: String,
    },
}
