//! Linear integration rows and operations (contract:
//! `docs/internal/plans/work-outline/linear-contract.md`).
//!
//! - `workOutlineSpaceSettings` (row id = Space id): `spaceId`,
//!   `linearEnabled` (LWW bool, default false), `updatedAt`, `v`. Created by
//!   one upsert, later writes are updates; deleted with the Space (it carries
//!   `spaceId`, so the cascade and the orphan sweep cover it).
//! - `workItems.externalSyncModifiedFields` (LWW JSON array of `"title"` /
//!   `"description"`): fields of an issue-linked item the user edited.
//! - `createWorkItemFromLinear`: the item and its `linear`/`issue` reference
//!   in one batch, replay-safe like `createWorkItem`.
//! - `linkLinearProject`: a `linear`/`project` reference (the ordinary
//!   reference mutator).
//!
//! The engine refuses the Space-scoped operations with
//! `linear/disabledForSpace` before they get here; this layer re-checks
//! under the registry lock ([`LINEAR_DISABLED_MESSAGE`], `invalid`).

use std::collections::BTreeMap;

use mast_proto::linear::{
    LINEAR_ISSUE_REFERENCE, LINEAR_PROJECT_REFERENCE, LINEAR_PROVIDER, linear_item_description,
    linear_item_title,
};
use mast_proto::work_outline::{
    CreateWorkItemExternalReferenceParams, CreateWorkItemFromLinearParams, LinkLinearProjectParams,
    SetSpaceLinearAccountParams, SetSpaceLinearEnabledParams, WorkItemStatus, WorkOutlineActor,
    WorkOutlineSpaceSettings, derive_id, external_reference_id, limits, roles,
};
use serde_json::{Value, json};

use super::materialize::{DecodedItem, str_field};
use super::{
    KIND_WORK_ITEM_EXTERNAL_REFERENCES, KIND_WORK_ITEMS, NewItem, Outcome, Planned,
    WORK_OUTLINE_ROW_VERSION, actor_row_value, check_id, check_operation_id, check_text,
    check_title, inputs_hash, invalid, not_found,
};
use crate::registry::{RegistryDoc, RegistryRow, RowState};

pub const KIND_WORK_OUTLINE_SPACE_SETTINGS: &str = "workOutlineSpaceSettings";
/// The `workItems` field holding the user-modified sync fields.
pub const MODIFIED_FIELDS_FIELD: &str = "externalSyncModifiedFields";
/// Message of the doc-layer backstop when a Space has Linear off.
pub const LINEAR_DISABLED_MESSAGE: &str = "Linear is turned off for this Space";

pub(crate) fn is_linear_issue(provider: &str, reference_type: &str) -> bool {
    provider == LINEAR_PROVIDER && reference_type == LINEAR_ISSUE_REFERENCE
}

/// `externalSyncModifiedFields` of an item row, sorted and deduplicated
/// (non-string entries and a non-array value are ignored).
pub(crate) fn decode_modified_fields(row: &RegistryRow) -> Vec<String> {
    let mut fields: Vec<String> = row
        .fields
        .get(MODIFIED_FIELDS_FIELD)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    fields.sort();
    fields.dedup();
    fields
}

fn decode_model(row: Option<&RegistryRow>, field: &str) -> Option<String> {
    row.and_then(|r| r.fields.get(field))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_owned)
}

/// The snapshot view of a Space's settings row (defaults when absent).
pub(crate) fn decode_space_settings(row: Option<&RegistryRow>) -> WorkOutlineSpaceSettings {
    WorkOutlineSpaceSettings {
        worker_worktrees: row.and_then(|r| r.fields.get("workerWorktrees")).and_then(Value::as_bool).unwrap_or(false),
        merged_pr_completion: row.and_then(|r| r.fields.get("mergedPrCompletion")).and_then(|v| serde_json::from_value(v.clone()).ok()),
        merged_pr_completion_since: row.and_then(|r| r.fields.get("mergedPrCompletionSince")).and_then(Value::as_i64),
        archive_workers_after_days: row.and_then(|r| r.fields.get("archiveWorkersAfterDays")).and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).filter(|v| *v > 0),
        resolve_merged_handoffs: row
            .and_then(|row| row.fields.get("resolveMergedHandoffs"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        // Absent keeps the default; a stored 0 means no cap.
        worker_context_window: match row.and_then(|r| r.fields.get("workerContextWindow")).and_then(Value::as_u64) {
            Some(0) => None,
            Some(tokens) => Some(tokens),
            None => mast_proto::work_outline::WorkOutlineSpaceSettings::default().worker_context_window,
        },
        // Absent keeps the default; a stored 0 means off.
        worker_call_nudge: row
            .and_then(|r| r.fields.get("workerCallNudge"))
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(mast_proto::work_outline::DEFAULT_WORKER_CALL_NUDGE),
        manager_default_model: decode_model(row, "managerDefaultModel"),
        worker_default_model: decode_model(row, "workerDefaultModel"),
        linear_enabled: row
            .and_then(|row| row.fields.get("linearEnabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        linear_account_id: row
            .and_then(|row| row.fields.get("linearAccountId"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        pr_review_sync_work_item_id: super::pr_review_sync::decode_destination(row),
        linear_assigned_work_item_id: super::pr_review_sync::decode_linear_assigned(row),
        pr_review_prompts: row
            .and_then(|r| r.fields.get("prReviewPrompts"))
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        approval_rules: Vec::new(),
        external_runs: row
            .and_then(|r| r.fields.get("externalRunSettings"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default(),
    }
}

/// `current ∪ add`, sorted; `None` when nothing new is added.
pub(crate) fn merge_modified_fields(current: &[String], add: &[&str]) -> Option<Vec<String>> {
    let mut fields: Vec<String> = current.to_vec();
    let mut changed = false;
    for field in add {
        if !fields.iter().any(|f| f == field) {
            fields.push((*field).to_owned());
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    fields.sort();
    Some(fields)
}

impl RegistryDoc {
    pub fn set_worker_automation(
        &mut self,
        p: &mast_proto::work_outline::SetWorkerAutomationParams,
        now_ms: i64,
    ) -> Outcome<()> {
        check_operation_id(&p.operation_id)?;
        self.require_space(&p.space_id)?;
        use mast_proto::work_outline::WorkerAutomationField;
        if p.field.is_none_or(|field| field == WorkerAutomationField::ArchiveWorkers) && p.archive_workers_after_days == Some(0) {
            return Err(invalid("The archive delay must be at least one day"));
        }
        let mut fields = BTreeMap::from([
            ("spaceId".to_owned(), json!(p.space_id)),
            ("updatedAt".to_owned(), json!(now_ms)),
            ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
        ]);
        if p.field.is_none_or(|field| field == WorkerAutomationField::Worktrees) {
            fields.insert("workerWorktrees".into(), json!(p.worker_worktrees));
        }
        if p.field.is_none_or(|field| field == WorkerAutomationField::MergedPrCompletion) {
            fields.insert("mergedPrCompletion".into(), json!(p.merged_pr_completion));
            fields.insert("mergedPrCompletionSince".into(), json!(p.merged_pr_completion.map(|_| now_ms)));
        }
        if p.field.is_none_or(|field| field == WorkerAutomationField::ArchiveWorkers) {
            fields.insert("archiveWorkersAfterDays".into(), json!(p.archive_workers_after_days));
        }
        let planned = match self.row_state(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id) {
            RowState::Live => Planned::update(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id, fields),
            RowState::Absent => Planned::upsert(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id, fields),
            RowState::Tombstoned => return Err(not_found("This Space's settings were deleted")),
        };
        self.commit(vec![planned], &[(super::KIND_SPACES, p.space_id.as_str())])
    }

    pub fn set_external_run_settings(
        &mut self,
        p: &mast_proto::SetExternalRunSettingsParams,
        now_ms: i64,
    ) -> Outcome<()> {
        check_operation_id(&p.operation_id)?;
        self.require_space(&p.space_id)?;
        validate_external_run_settings(&p.settings)?;
        let fields = BTreeMap::from([
            ("spaceId".to_owned(), json!(p.space_id)),
            ("externalRunSettings".to_owned(), json!(p.settings)),
            ("updatedAt".to_owned(), json!(now_ms)),
            ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
        ]);
        let planned = match self.row_state(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id) {
            RowState::Live => Planned::update(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id, fields),
            RowState::Absent => Planned::upsert(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id, fields),
            RowState::Tombstoned => return Err(not_found("This Space's settings were deleted")),
        };
        self.commit(vec![planned], &[(super::KIND_SPACES, p.space_id.as_str())])
    }

    pub fn set_resolve_merged_handoffs(
        &mut self,
        p: &mast_proto::work_outline::SetResolveMergedHandoffsParams,
        now_ms: i64,
    ) -> Outcome<()> {
        check_operation_id(&p.operation_id)?;
        self.require_space(&p.space_id)?;
        let planned = match self.row_state(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id) {
            RowState::Live => {
                if decode_space_settings(
                    self.overlay_row(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id)
                        .as_ref(),
                )
                .resolve_merged_handoffs
                    == p.enabled
                {
                    return Ok(());
                }
                Planned::update(
                    KIND_WORK_OUTLINE_SPACE_SETTINGS,
                    &p.space_id,
                    BTreeMap::from([
                        ("resolveMergedHandoffs".to_owned(), json!(p.enabled)),
                        ("updatedAt".to_owned(), json!(now_ms)),
                    ]),
                )
            }
            RowState::Absent => {
                if p.enabled {
                    return Ok(());
                }
                Planned::upsert(
                    KIND_WORK_OUTLINE_SPACE_SETTINGS,
                    &p.space_id,
                    BTreeMap::from([
                        ("spaceId".to_owned(), json!(p.space_id)),
                        ("resolveMergedHandoffs".to_owned(), json!(p.enabled)),
                        ("updatedAt".to_owned(), json!(now_ms)),
                        ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
                    ]),
                )
            }
            RowState::Tombstoned => return Err(not_found("This Space's settings were deleted")),
        };
        self.commit(vec![planned], &[(super::KIND_SPACES, p.space_id.as_str())])
    }

    /// Default stores null (so a later change of the default reaches the
    /// Space), no cap stores 0.
    pub fn set_worker_context_window(
        &mut self,
        p: &mast_proto::work_outline::SetWorkerContextWindowParams,
        now_ms: i64,
    ) -> Outcome<()> {
        use mast_proto::work_outline::WORKER_CONTEXT_WINDOW_SIZES;
        check_operation_id(&p.operation_id)?;
        self.require_space(&p.space_id)?;
        if let Some(tokens) = p.tokens
            && tokens != 0
            && !WORKER_CONTEXT_WINDOW_SIZES.contains(&tokens)
        {
            return Err(invalid(
                "The worker context window must be the default, no cap, or one of the offered sizes",
            ));
        }
        let planned = match self.row_state(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id) {
            RowState::Live => Planned::update(
                KIND_WORK_OUTLINE_SPACE_SETTINGS,
                &p.space_id,
                BTreeMap::from([
                    ("workerContextWindow".to_owned(), json!(p.tokens)),
                    ("updatedAt".to_owned(), json!(now_ms)),
                ]),
            ),
            RowState::Absent => {
                if p.tokens.is_none() {
                    return Ok(());
                }
                Planned::upsert(
                    KIND_WORK_OUTLINE_SPACE_SETTINGS,
                    &p.space_id,
                    BTreeMap::from([
                        ("spaceId".to_owned(), json!(p.space_id)),
                        ("workerContextWindow".to_owned(), json!(p.tokens)),
                        ("updatedAt".to_owned(), json!(now_ms)),
                        ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
                    ]),
                )
            }
            RowState::Tombstoned => return Err(not_found("This Space's settings were deleted")),
        };
        self.commit(vec![planned], &[(super::KIND_SPACES, p.space_id.as_str())])
    }

    /// Default stores null (so a later change of the default reaches the
    /// Space), off stores 0.
    pub fn set_worker_call_nudge(
        &mut self,
        p: &mast_proto::work_outline::SetWorkerCallNudgeParams,
        now_ms: i64,
    ) -> Outcome<()> {
        use mast_proto::work_outline::WORKER_CALL_NUDGE_CHOICES;
        check_operation_id(&p.operation_id)?;
        self.require_space(&p.space_id)?;
        if let Some(calls) = p.calls
            && calls != 0
            && !WORKER_CALL_NUDGE_CHOICES.contains(&calls)
        {
            return Err(invalid(
                "The worker call nudge must be the default, off, or one of the offered counts",
            ));
        }
        let planned = match self.row_state(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id) {
            RowState::Live => Planned::update(
                KIND_WORK_OUTLINE_SPACE_SETTINGS,
                &p.space_id,
                BTreeMap::from([
                    ("workerCallNudge".to_owned(), json!(p.calls)),
                    ("updatedAt".to_owned(), json!(now_ms)),
                ]),
            ),
            RowState::Absent => {
                if p.calls.is_none() {
                    return Ok(());
                }
                Planned::upsert(
                    KIND_WORK_OUTLINE_SPACE_SETTINGS,
                    &p.space_id,
                    BTreeMap::from([
                        ("spaceId".to_owned(), json!(p.space_id)),
                        ("workerCallNudge".to_owned(), json!(p.calls)),
                        ("updatedAt".to_owned(), json!(now_ms)),
                        ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
                    ]),
                )
            }
            RowState::Tombstoned => return Err(not_found("This Space's settings were deleted")),
        };
        self.commit(vec![planned], &[(super::KIND_SPACES, p.space_id.as_str())])
    }

    /// Absent or empty stores null (the built-in default, so a later change of
    /// it reaches the Space).
    pub fn set_role_default_model(
        &mut self,
        p: &mast_proto::work_outline::SetRoleDefaultModelParams,
        now_ms: i64,
    ) -> Outcome<()> {
        use mast_proto::work_outline::DefaultModelRole;
        check_operation_id(&p.operation_id)?;
        self.require_space(&p.space_id)?;
        let model = p.model.as_deref().map(str::trim).filter(|m| !m.is_empty());
        if model.is_some_and(|m| m.len() > 200 || m.chars().any(char::is_control)) {
            return Err(invalid("The model id is not valid"));
        }
        let field = match p.role {
            DefaultModelRole::Manager => "managerDefaultModel",
            DefaultModelRole::Worker => "workerDefaultModel",
        };
        let planned = match self.row_state(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id) {
            RowState::Live => Planned::update(
                KIND_WORK_OUTLINE_SPACE_SETTINGS,
                &p.space_id,
                BTreeMap::from([
                    (field.to_owned(), json!(model)),
                    ("updatedAt".to_owned(), json!(now_ms)),
                ]),
            ),
            RowState::Absent => {
                if model.is_none() {
                    return Ok(());
                }
                Planned::upsert(
                    KIND_WORK_OUTLINE_SPACE_SETTINGS,
                    &p.space_id,
                    BTreeMap::from([
                        ("spaceId".to_owned(), json!(p.space_id)),
                        (field.to_owned(), json!(model)),
                        ("updatedAt".to_owned(), json!(now_ms)),
                        ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
                    ]),
                )
            }
            RowState::Tombstoned => return Err(not_found("This Space's settings were deleted")),
        };
        self.commit(vec![planned], &[(super::KIND_SPACES, p.space_id.as_str())])
    }

    /// Whether the Space has Linear turned on (its settings row says so).
    pub fn space_linear_enabled(&self, space_id: &str) -> bool {
        decode_space_settings(
            self.overlay_row(KIND_WORK_OUTLINE_SPACE_SETTINGS, space_id)
                .as_ref(),
        )
        .linear_enabled
    }

    /// The account selected by a Space, even while Linear is disabled.
    pub fn space_linear_account_id(&self, space_id: &str) -> Option<String> {
        let row = self.overlay_row(KIND_WORK_OUTLINE_SPACE_SETTINGS, space_id);
        decode_space_settings(row.as_ref()).linear_account_id
    }

    /// The selected account for an enabled Space; `None` means disabled.
    pub fn space_linear_account(&self, space_id: &str) -> Option<Option<String>> {
        let row = self.overlay_row(KIND_WORK_OUTLINE_SPACE_SETTINGS, space_id);
        let settings = decode_space_settings(row.as_ref());
        settings.linear_enabled.then_some(settings.linear_account_id)
    }

    /// Live Spaces with Linear turned on (sorted).
    pub fn linear_enabled_space_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .overlay_rows(KIND_WORK_OUTLINE_SPACE_SETTINGS)
            .into_iter()
            .filter(|row| decode_space_settings(Some(row)).linear_enabled)
            .filter_map(|row| str_field(&row, "spaceId").or(Some(row.id)))
            .filter(|space| self.row_state(super::KIND_SPACES, space) == RowState::Live)
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Live Linear issue references of an item (ids), optionally ignoring one.
    fn linear_issue_reference_ids(&self, work_item_id: &str, except: Option<&str>) -> Vec<String> {
        self.overlay_rows(KIND_WORK_ITEM_EXTERNAL_REFERENCES)
            .into_iter()
            .filter(|row| Some(row.id.as_str()) != except)
            .filter(|row| str_field(row, "workItemId").as_deref() == Some(work_item_id))
            .filter(|row| {
                is_linear_issue(
                    &str_field(row, "provider").unwrap_or_default(),
                    &str_field(row, "refType").unwrap_or_default(),
                )
            })
            .map(|row| row.id)
            .collect()
    }

    /// The item's modified fields plus `edited` when the item is
    /// issue-linked and something new is added; `None` otherwise.
    pub(super) fn modified_fields_with(
        &self,
        item: &DecodedItem,
        edited: &[&str],
    ) -> Option<Vec<String>> {
        if edited.is_empty() || self.linear_issue_reference_ids(&item.id, None).is_empty() {
            return None;
        }
        merge_modified_fields(&item.external_sync_modified_fields, edited)
    }

    /// When removing `reference_id` unlinks an item's last Linear issue and
    /// the item has modified fields: the item id (to clear them).
    pub(super) fn clears_modified_fields_on_unlink(&self, reference_id: &str) -> Option<String> {
        let row = self.overlay_row(KIND_WORK_ITEM_EXTERNAL_REFERENCES, reference_id)?;
        if !is_linear_issue(
            &str_field(&row, "provider").unwrap_or_default(),
            &str_field(&row, "refType").unwrap_or_default(),
        ) {
            return None;
        }
        let work_item_id = str_field(&row, "workItemId")?;
        let item = self.live_item(&work_item_id).ok()?;
        if item.external_sync_modified_fields.is_empty()
            || !self
                .linear_issue_reference_ids(&work_item_id, Some(reference_id))
                .is_empty()
        {
            return None;
        }
        Some(work_item_id)
    }

    pub(super) fn require_linear_enabled(&self, space_id: &str) -> Outcome<()> {
        if self.space_linear_enabled(space_id) {
            Ok(())
        } else {
            Err(invalid(LINEAR_DISABLED_MESSAGE))
        }
    }

    /// `setSpaceLinearEnabled`: LWW write of the Space's settings row (no
    /// write when the value already holds).
    pub fn set_space_linear_enabled(
        &mut self,
        p: &SetSpaceLinearEnabledParams,
        now_ms: i64,
    ) -> Outcome<()> {
        check_operation_id(&p.operation_id)?;
        self.require_space(&p.space_id)?;
        let planned = match self.row_state(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id) {
            RowState::Live => {
                if self.space_linear_enabled(&p.space_id) == p.enabled {
                    return Ok(());
                }
                Planned::update(
                    KIND_WORK_OUTLINE_SPACE_SETTINGS,
                    &p.space_id,
                    BTreeMap::from([
                        ("linearEnabled".to_owned(), json!(p.enabled)),
                        ("updatedAt".to_owned(), json!(now_ms)),
                    ]),
                )
            }
            RowState::Absent => {
                if !p.enabled {
                    return Ok(());
                }
                Planned::upsert(
                    KIND_WORK_OUTLINE_SPACE_SETTINGS,
                    &p.space_id,
                    BTreeMap::from([
                        ("spaceId".to_owned(), json!(p.space_id)),
                        ("linearEnabled".to_owned(), json!(p.enabled)),
                        ("updatedAt".to_owned(), json!(now_ms)),
                        ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
                    ]),
                )
            }
            RowState::Tombstoned => return Err(not_found("This Space's settings were deleted")),
        };
        self.commit(vec![planned], &[(super::KIND_SPACES, p.space_id.as_str())])
    }

    /// Select the Space's Linear account using the settings row's LWW field.
    pub fn set_space_linear_account(
        &mut self,
        p: &SetSpaceLinearAccountParams,
        now_ms: i64,
    ) -> Outcome<()> {
        check_operation_id(&p.operation_id)?;
        self.require_space(&p.space_id)?;
        if let Some(account_id) = &p.account_id {
            check_id("accountId", account_id)?;
        }
        let row = self.overlay_row(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id);
        if decode_space_settings(row.as_ref()).linear_account_id == p.account_id {
            return Ok(());
        }
        let planned = match self.row_state(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id) {
            RowState::Live => Planned::update(
                KIND_WORK_OUTLINE_SPACE_SETTINGS,
                &p.space_id,
                BTreeMap::from([
                    ("linearAccountId".to_owned(), json!(p.account_id)),
                    ("updatedAt".to_owned(), json!(now_ms)),
                ]),
            ),
            RowState::Absent => Planned::upsert(
                KIND_WORK_OUTLINE_SPACE_SETTINGS,
                &p.space_id,
                BTreeMap::from([
                    ("spaceId".to_owned(), json!(p.space_id)),
                    ("linearAccountId".to_owned(), json!(p.account_id)),
                    ("updatedAt".to_owned(), json!(now_ms)),
                    ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
                ]),
            ),
            RowState::Tombstoned => return Err(not_found("This Space's settings were deleted")),
        };
        self.commit(vec![planned], &[(super::KIND_SPACES, p.space_id.as_str())])
    }

    /// `createWorkItemFromLinear`: the item (`"ENG-123 · title"`, Linear's
    /// description) and its `linear`/`issue` reference in ONE batch. Returns
    /// `(workItemId, referenceId)`; a replay with the same inputs writes
    /// nothing and returns the same ids.
    pub fn create_work_item_from_linear(
        &mut self,
        p: &CreateWorkItemFromLinearParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<(String, String)> {
        check_operation_id(&p.operation_id)?;
        let identifier = p.issue.identifier.trim();
        check_text(
            "Issue identifier",
            identifier,
            limits::MAX_EXTERNAL_ID_CHARS,
            true,
        )?;
        check_text("URL", &p.issue.url, limits::MAX_REFERENCE_URL_CHARS, true)?;
        let title = check_title(&linear_item_title(identifier, &p.issue.title))?;
        let description = linear_item_description(p.issue.description.as_deref());
        let id = derive_id(&p.operation_id, roles::ITEM);
        let reference_id =
            external_reference_id(&id, LINEAR_PROVIDER, LINEAR_ISSUE_REFERENCE, identifier);
        let hash = inputs_hash(p);
        if self.created_item_replay(&id, &hash)? {
            return Ok((id, reference_id));
        }
        self.require_space(&p.space_id)?;
        self.require_linear_enabled(&p.space_id)?;
        let parent = p.parent_work_item_id.as_deref();
        let mut plan = self.plan_new_item(
            NewItem {
                id: &id,
                space_id: &p.space_id,
                parent,
                placement: &p.placement,
                title: &title,
                description: description.as_deref(),
                status: WorkItemStatus::Pending,
                inputs_hash: &hash,
            },
            actor,
            now_ms,
        )?;
        self.plan_follow_for_new_child(&p.space_id, parent, &id, actor, now_ms, &mut plan)?;
        let mut set = BTreeMap::from([
            ("spaceId".to_owned(), json!(p.space_id)),
            ("workItemId".to_owned(), json!(id)),
            ("provider".to_owned(), json!(LINEAR_PROVIDER)),
            ("refType".to_owned(), json!(LINEAR_ISSUE_REFERENCE)),
            ("externalId".to_owned(), json!(identifier)),
            ("url".to_owned(), json!(p.issue.url)),
            ("createdBy".to_owned(), actor_row_value(actor)),
            ("createdAt".to_owned(), json!(now_ms)),
            ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
        ]);
        let cached: String = p
            .issue
            .title
            .trim()
            .chars()
            .take(limits::MAX_REFERENCE_TITLE_CHARS)
            .collect();
        if !cached.is_empty() {
            set.insert("cachedTitle".into(), json!(cached));
        }
        plan.push(Planned::upsert(
            KIND_WORK_ITEM_EXTERNAL_REFERENCES,
            &reference_id,
            set,
        ));
        let extra: Vec<(&str, &str)> = parent.map(|p| (KIND_WORK_ITEMS, p)).into_iter().collect();
        self.commit(plan, &extra)?;
        Ok((id, reference_id))
    }

    /// `linkLinearProject`: a `linear`/`project` reference on an existing
    /// item (content-derived id, so a repeat converges to one row).
    pub fn link_linear_project(
        &mut self,
        p: &LinkLinearProjectParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<String> {
        check_operation_id(&p.operation_id)?;
        check_id("work item", &p.work_item_id)?;
        let item = self.live_item(&p.work_item_id)?;
        self.require_space(&item.space_id)?;
        self.require_linear_enabled(&item.space_id)?;
        let name: String = p
            .project
            .name
            .trim()
            .chars()
            .take(limits::MAX_REFERENCE_TITLE_CHARS)
            .collect();
        self.create_work_item_external_reference(
            &CreateWorkItemExternalReferenceParams {
                work_item_id: p.work_item_id.clone(),
                provider: LINEAR_PROVIDER.into(),
                reference_type: LINEAR_PROJECT_REFERENCE.into(),
                external_id: p.project.id.trim().to_owned(),
                url: p.project.url.clone(),
                cached_title: (!name.is_empty()).then_some(name),
                operation_id: p.operation_id.clone(),
            },
            actor,
            now_ms,
        )
    }
}

#[cfg(test)]
mod tests;

const MAX_RUN_SETTING_CHARS: usize = 300;

/// The conventions end up in a script run on a remote box, so they are held to
/// a plain character set here and checked again after `{runTag}` is filled in.
fn validate_external_run_settings(
    settings: &mast_proto::ExternalRunSettings,
) -> Outcome<()> {
    fn plain(text: &str) -> bool {
        text.chars().count() <= MAX_RUN_SETTING_CHARS
            && text.chars().all(|c| c.is_ascii_alphanumeric() || "._-/{}".contains(c))
    }
    if !(1..=100).contains(&settings.capacity_cap) {
        return Err(invalid("The capacity cap must be between 1 and 100"));
    }
    if settings.box_limit.is_some_and(|limit| !(1..=1000).contains(&limit)) {
        return Err(invalid("The box limit must be between 1 and 1000"));
    }
    if settings.stale_days == 0 {
        return Err(invalid("Stale days must be at least one"));
    }
    if let Some(dir) = &settings.run_dir
        && (!dir.starts_with('/') || !plain(dir) || dir.contains(".."))
    {
        return Err(invalid("The run directory must be an absolute path of plain characters"));
    }
    for (label, file) in [("exit file", &settings.exit_file), ("output file", &settings.output_file)] {
        if let Some(file) = file
            && (file.is_empty() || file.contains('/') || file.contains("..") || !plain(file))
        {
            return Err(invalid(format!("The {label} must be a plain file name")));
        }
    }
    if let Some(session) = &settings.tmux_session
        && (session.is_empty() || session.contains('/') || !plain(session))
    {
        return Err(invalid("The tmux session must be a plain name"));
    }
    Ok(())
}
