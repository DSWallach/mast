use super::*;
use mast_proto::{
    PendingStandingHandOff, StandingRole, StandingRoleKind, StandingRoleModel, StandingRoleState,
    standing_role_id,
};

pub const KIND_STANDING_ROLES: &str = "standingRoles";

/// Newest chain entries kept on the role row so its op stays far under the
/// registry op size limit. Superseded chats keep their own
/// `standingRoleSuccessor` marker, so dropping old ids loses no visibility.
const MAX_CHAIN: usize = 64;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStandingRole {
    id: String,
    role: StandingRoleKind,
    scope_item_id: String,
    #[serde(default)]
    current_chat_id: Option<String>,
    #[serde(default)]
    chain: Vec<String>,
    #[serde(default)]
    fallback_chain: Vec<StandingRoleModel>,
    #[serde(default)]
    state: StandingRoleState,
    #[serde(default)]
    worker_model: Option<StandingRoleModel>,
    #[serde(default)]
    read_only: Option<bool>,
    /// Recent `setManagerReadOnly` operation ids, so a retry replays once.
    #[serde(default)]
    read_only_ops: Vec<String>,
    #[serde(default)]
    pending_hand_off: Option<PendingStandingHandOff>,
}

impl From<RawStandingRole> for StandingRole {
    fn from(raw: RawStandingRole) -> Self {
        Self {
            id: raw.id,
            role: raw.role,
            scope_item_id: raw.scope_item_id,
            current_chat_id: raw.current_chat_id,
            chain: raw.chain,
            fallback_chain: raw.fallback_chain,
            state: raw.state,
            worker_model: raw.worker_model,
            read_only: raw.read_only,
        }
    }
}

impl RegistryDoc {
    pub fn standing_role(
        &self,
        role: StandingRoleKind,
        scope_item_id: &str,
    ) -> Option<StandingRole> {
        self.standing_role_by_id(&standing_role_id(role, scope_item_id))
    }

    fn standing_role_by_id(&self, id: &str) -> Option<StandingRole> {
        self.overlay_row(KIND_STANDING_ROLES, id)
            .and_then(|row| row_to::<RawStandingRole>(&row))
            .map(StandingRole::from)
    }

    pub fn standing_roles(&self) -> Vec<StandingRole> {
        let mut roles: Vec<StandingRole> = self
            .read_kind::<RawStandingRole>(KIND_STANDING_ROLES)
            .into_iter()
            .map(StandingRole::from)
            .collect();
        roles.sort_by(|a, b| a.id.cmp(&b.id));
        roles
    }

    /// The chat currently holding the role: the one answer every caller uses
    /// instead of remembering a chat id.
    pub fn standing_role_chat(
        &self,
        role: StandingRoleKind,
        scope_item_id: &str,
    ) -> Option<String> {
        self.standing_role(role, scope_item_id)?.current_chat_id
    }

    /// The role `chat_id` currently holds, if any. A superseded chat holds
    /// nothing.
    pub fn standing_role_of_chat(&self, chat_id: &str) -> Option<StandingRole> {
        // Decode and convert only the match: this runs on every agent call.
        self.read_kind::<RawStandingRole>(KIND_STANDING_ROLES)
            .into_iter()
            .find(|raw| raw.current_chat_id.as_deref() == Some(chat_id))
            .map(StandingRole::from)
    }

    /// Whether `chat_id` is, or is about to become, a project's Manager: the
    /// role's current chat, the successor a reserved hand-off names, or a
    /// provisional first start of a Manager, which the role adopts only once
    /// its first message is queued. All count, so a Manager's first run is
    /// restricted before anything has adopted it.
    pub fn chat_is_manager(&self, chat_id: &str) -> bool {
        let held = self
            .read_kind::<RawStandingRole>(KIND_STANDING_ROLES)
            .into_iter()
            .any(|raw| {
                raw.role == StandingRoleKind::Manager
                    && (raw.current_chat_id.as_deref() == Some(chat_id)
                        || raw
                            .pending_hand_off
                            .as_ref()
                            .is_some_and(|pending| pending.to_chat_id == chat_id))
            });
        held || self.is_provisional_manager(chat_id)
    }

    /// Whether `chat_id` is a Manager whose runs are read-only: the setting of
    /// the role it holds, will hold (a reserved hand-off successor) or is
    /// first starting, read now. A Manager of a project that never chose is
    /// read-only; a chat that is no Manager is not.
    pub fn manager_runs_read_only(&self, chat_id: &str) -> bool {
        if !self.chat_is_manager(chat_id) {
            return false;
        }
        let provisional_role_id = self
            .chat(chat_id)
            .ok()
            .flatten()
            .and_then(|chat| {
                chat.standing_role_provisional()
                    .and_then(|tag| tag.strip_prefix(mast_proto::STANDING_ROLE_PROVISIONAL_PREFIX).map(str::to_owned))
            })
            .map(|rest| rest.split('|').next().unwrap_or_default().to_owned());
        let role = self
            .read_kind::<RawStandingRole>(KIND_STANDING_ROLES)
            .into_iter()
            .find(|raw| {
                raw.role == StandingRoleKind::Manager
                    && (raw.current_chat_id.as_deref() == Some(chat_id)
                        || raw.pending_hand_off.as_ref().is_some_and(|pending| pending.to_chat_id == chat_id)
                        || provisional_role_id.as_deref() == Some(raw.id.as_str()))
            })
            .map(StandingRole::from);
        StandingRole::manager_runs_read_only(role.as_ref())
    }

    /// Set whether a project's Manager runs read-only, creating the role's
    /// placeholder record when the project has no Manager yet. An operation id
    /// seen before applies nothing, even after a later change.
    pub fn set_manager_read_only(
        &mut self,
        scope_item_id: &str,
        read_only: bool,
        operation_id: &str,
    ) -> Result<(), String> {
        const KEEP_OPS: usize = 16;
        let id = standing_role_id(StandingRoleKind::Manager, scope_item_id);
        let existing = self
            .overlay_row(KIND_STANDING_ROLES, &id)
            .and_then(|row| row_to::<RawStandingRole>(&row));
        let mut ops = existing.as_ref().map(|raw| raw.read_only_ops.clone()).unwrap_or_default();
        if ops.iter().any(|seen| seen == operation_id) {
            return Ok(());
        }
        ops.push(operation_id.to_owned());
        if ops.len() > KEEP_OPS {
            ops.drain(..ops.len() - KEEP_OPS);
        }
        let (op, mut set) = match existing {
            Some(_) => (OpKind::Update, BTreeMap::new()),
            None => (
                OpKind::Upsert,
                role_fields(&StandingRole {
                    id: id.clone(),
                    role: StandingRoleKind::Manager,
                    scope_item_id: scope_item_id.to_owned(),
                    current_chat_id: None,
                    chain: Vec::new(),
                    fallback_chain: Vec::new(),
                    state: StandingRoleState::Idle,
                    worker_model: None,
                    read_only: None,
                }),
            ),
        };
        set.insert("readOnly".to_owned(), json!(read_only));
        set.insert("readOnlyOps".to_owned(), json!(ops));
        self.commit_rows(vec![(KIND_STANDING_ROLES, id, op, set)])
    }

    fn is_provisional_manager(&self, chat_id: &str) -> bool {
        let manager_prefix = mast_proto::standing_role_provisional_tag(StandingRoleKind::Manager, "");
        self.chat(chat_id)
            .ok()
            .flatten()
            .is_some_and(|chat| {
                chat.standing_role_provisional()
                    .is_some_and(|tag| tag.starts_with(&manager_prefix))
            })
    }

    /// Write rows as one checked registry batch: every op is size-gated first
    /// and nothing is enqueued unless all pass, and the ops go out together,
    /// so a second device never sees half of a role change.
    pub(super) fn commit_rows(
        &mut self,
        rows: Vec<RegistryUpdate>,
    ) -> Result<(), String> {
        // Every affected row is observed before the one shared clock is
        // minted (rule R3), so the clock beats the newest clock on any of
        // them. Minting first would let a row with a newer synced or restored
        // clock reject its half of the batch while the other half applies.
        for (kind, id, _, _) in &rows {
            self.observe_row(kind, id);
        }
        let hlc = self.mint_hlc();
        let mut ops = Vec::with_capacity(rows.len());
        for (kind, id, op, set) in rows {
            ops.push(RowOp {
                kind: kind.to_owned(),
                id,
                op,
                set: Some(set),
                hlc: hlc.clone(),
                clocks: None,
            });
        }
        self.write_ops(ops).map_err(|e| e.to_string())
    }

    fn pending_hand_off_of(&self, role_id: &str) -> Option<PendingStandingHandOff> {
        self.overlay_row(KIND_STANDING_ROLES, role_id)
            .and_then(|row| row_to::<RawStandingRole>(&row))
            .and_then(|raw| raw.pending_hand_off)
    }

    /// The hand-off reserved on this role and not yet committed, if any.
    pub fn pending_standing_hand_off(
        &self,
        role: StandingRoleKind,
        scope_item_id: &str,
    ) -> Option<PendingStandingHandOff> {
        self.pending_hand_off_of(&standing_role_id(role, scope_item_id))
    }

    /// Whether the hand-off `from -> to` already moved workers: the point
    /// after which it must be finished by retrying it, not replaced.
    pub fn hand_off_recorded(&self, from: &str, to: &str) -> bool {
        self.overlay_row(KIND_CHATS, to)
            .and_then(|row| row.fields.get("orchestratorHandOff").cloned())
            .is_some_and(|p| p.get("fromChatId").and_then(Value::as_str) == Some(from))
    }

    /// Make `chat_id` the role's chat, creating the record on first use.
    /// Replays of the same adoption succeed. Refused: a different live
    /// holder (rotation is the only way to change holders), a chat that
    /// already holds another role, a scope item that is missing or in another
    /// Space, a chat whose host does not own its Space, and an oversized
    /// fallback chain.
    pub fn adopt_standing_role(
        &mut self,
        role: StandingRoleKind,
        scope_item_id: &str,
        chat_id: &str,
        fallback_chain: &[StandingRoleModel],
    ) -> Result<StandingRole, String> {
        self.adopt_standing_role_inner(role, scope_item_id, chat_id, fallback_chain, false)
    }

    /// Adopt the role's provisional first-start chat: the same checks as
    /// [`Self::adopt_standing_role`], plus the chat must carry this role's
    /// provisional marker, which the same batch clears so the chat appears
    /// as the role's chat in one step.
    pub fn activate_standing_role_chat(
        &mut self,
        role: StandingRoleKind,
        scope_item_id: &str,
        chat_id: &str,
        fallback_chain: &[StandingRoleModel],
    ) -> Result<StandingRole, String> {
        self.adopt_standing_role_inner(role, scope_item_id, chat_id, fallback_chain, true)
    }

    fn adopt_standing_role_inner(
        &mut self,
        role: StandingRoleKind,
        scope_item_id: &str,
        chat_id: &str,
        fallback_chain: &[StandingRoleModel],
        activate_provisional: bool,
    ) -> Result<StandingRole, String> {
        let chat = self
            .chat(chat_id)
            .map_err(|e| e.to_string())?
            .ok_or("chat does not exist")?;
        let id = standing_role_id(role, scope_item_id);
        let existing = self.standing_role_by_id(&id);
        if let Some(existing) = &existing
            && existing.current_chat_id.as_deref() == Some(chat_id)
        {
            return Ok(existing.clone());
        }
        StandingRoleModel::validate_chain(fallback_chain)?;
        let space_id = chat
            .space_id
            .clone()
            .ok_or("a role chat belongs to a Space")?;
        match self.work_item_space(scope_item_id) {
            Some(scope_space) if scope_space == space_id => {}
            Some(_) => return Err("the scope item belongs to another Space".into()),
            None => return Err("the scope item does not exist".into()),
        }
        let owner = self
            .space(&space_id)
            .map_err(|e| e.to_string())?
            .ok_or("the chat's Space does not exist")?;
        if owner.device_id != chat.device_id {
            return Err("the chat's host does not own its Space".into());
        }
        if self.standing_role_of_chat(chat_id).is_some() {
            return Err("this chat already holds another role".into());
        }
        let provisional = chat.is_provisional_for(role, scope_item_id);
        if activate_provisional && !provisional {
            return Err("only this role's provisional first-start chat can be activated".into());
        }
        if chat.archived || (chat.is_standing_role_predecessor() && !activate_provisional) {
            return Err("an archived or superseded chat cannot hold a role".into());
        }
        let record = match existing {
            Some(existing) if existing.current_chat_id.is_some() => {
                return Err("role already has a different chat; rotate it instead".into());
            }
            Some(existing) => existing,
            None => StandingRole {
                id: id.clone(),
                role,
                scope_item_id: scope_item_id.to_owned(),
                current_chat_id: None,
                chain: Vec::new(),
                fallback_chain: fallback_chain.to_vec(),
                state: StandingRoleState::Idle,
                worker_model: None,
                read_only: None,
            },
        };
        let record = StandingRole {
            current_chat_id: Some(chat_id.to_owned()),
            chain: vec![chat_id.to_owned()],
            ..record
        };
        let mut rows = vec![(
            KIND_STANDING_ROLES,
            id,
            OpKind::Upsert,
            role_fields(&record),
        )];
        if activate_provisional {
            rows.push((
                KIND_CHATS,
                chat_id.to_owned(),
                OpKind::Update,
                fields([("standingRoleSuccessor", Value::Null)]),
            ));
        }
        self.commit_rows(rows)?;
        Ok(record)
    }

    fn check_rotation_destination(
        &self,
        source: &Chat,
        to: &str,
        to_space: Option<&str>,
    ) -> Result<(), String> {
        let (unusable, space) = match self.chat(to).map_err(|e| e.to_string())? {
            Some(destination) => (
                destination.archived || destination.is_standing_role_predecessor(),
                destination.space_id,
            ),
            None => (false, to_space.map(str::to_owned)),
        };
        if unusable {
            return Err("an archived or superseded chat cannot take over a role".into());
        }
        if source.space_id.is_none() || source.space_id != space {
            return Err("chats must belong to the same Space".into());
        }
        // The same invariants adoption enforces: one role per chat, and the
        // chat lives on the device that owns the Space.
        if let Some(destination) = self.chat(to).map_err(|e| e.to_string())? {
            if self.standing_role_of_chat(to).is_some() {
                return Err("the destination chat already holds a role".into());
            }
            let owner = self
                .space(space.as_deref().unwrap_or_default())
                .map_err(|e| e.to_string())?
                .ok_or("the chat's Space does not exist")?;
            if owner.device_id != destination.device_id {
                return Err("the destination chat's host does not own its Space".into());
            }
        }
        Ok(())
    }

    /// What handing `from` off to `to` would do to a standing role, without
    /// writing. `Ok(None)`: nothing to rotate (an ordinary chat, or a retry
    /// of a hand-off whose role has since moved on). `Ok(Some((role, true)))`:
    /// the role already moved. Errors: `from` was already replaced by some
    /// other chat, or the destination cannot take the role. `to_space` stands
    /// in for a destination chat that does not exist yet.
    fn plan_standing_role_hand_off(
        &self,
        from: &str,
        to: &str,
        to_space: Option<&str>,
    ) -> Result<Option<(StandingRole, bool)>, String> {
        if let Some(role) = self
            .standing_roles()
            .into_iter()
            .find(|role| role.replays_rotation(from, to))
        {
            return Ok(Some((role, true)));
        }
        let source = self
            .chat(from)
            .map_err(|e| e.to_string())?
            .ok_or("source chat does not exist")?;
        if let Some(next) = source.standing_role_successor.as_deref() {
            return if next == to {
                Ok(None)
            } else {
                Err(format!(
                    "the source chat was already replaced by chat {next}"
                ))
            };
        }
        let Some(role) = self.standing_role_of_chat(from) else {
            return Ok(None);
        };
        if from == to {
            return Err("a chat cannot take over its own role".into());
        }
        if let Some(pending) = self.pending_hand_off_of(&role.id)
            && pending.to_chat_id != to
            && self.hand_off_recorded(&pending.from_chat_id, &pending.to_chat_id)
        {
            return Err(format!(
                "a hand-off to chat {} is part-done; retry it to finish",
                pending.to_chat_id
            ));
        }
        self.check_rotation_destination(&source, to, to_space)?;
        Ok(Some((role, false)))
    }

    /// Persist the source's choice of successor before the launch creates,
    /// moves or queues anything. The reservation blocks a different successor
    /// only once the hand-off has moved workers (until then it is replaceable,
    /// since nothing irreversible happened); the rotation clears it.
    /// Ordinary chats and replays reserve nothing.
    pub fn reserve_standing_role_hand_off(
        &mut self,
        from: &str,
        to: &str,
        operation_id: &str,
        to_space: Option<&str>,
    ) -> Result<(), String> {
        let Some((role, false)) = self.plan_standing_role_hand_off(from, to, to_space)? else {
            return Ok(());
        };
        let pending = PendingStandingHandOff {
            from_chat_id: from.to_owned(),
            to_chat_id: to.to_owned(),
            operation_id: operation_id.to_owned(),
        };
        if role.state == StandingRoleState::HandingOff
            && self.pending_hand_off_of(&role.id).as_ref() == Some(&pending)
        {
            return Ok(());
        }
        let mut rows = Vec::new();
        // Replacing a reservation whose successor was created but never got
        // workers or a turn: hide that provisional chat in the same batch, so
        // it does not linger as a stray empty session.
        if let Some(old) = self.pending_hand_off_of(&role.id)
            && old.to_chat_id != to
            && !self.hand_off_recorded(&old.from_chat_id, &old.to_chat_id)
            && let Some(provisional) = self.chat(&old.to_chat_id).map_err(|e| e.to_string())?
            && !provisional.archived
        {
            rows.push((
                KIND_CHATS,
                old.to_chat_id,
                OpKind::Update,
                fields([("archived", json!(true))]),
            ));
        }
        rows.push((
            KIND_STANDING_ROLES,
            role.id,
            OpKind::Update,
            fields([
                ("state", json!(StandingRoleState::HandingOff)),
                ("pendingHandOff", json!(pending)),
            ]),
        ));
        self.commit_rows(rows)
    }

    /// The part-done hand-off reserved on the role `from` holds: workers
    /// already moved to its successor, the role has not. A new request from
    /// `from` that carries the same inputs resumes this operation.
    pub fn part_done_hand_off_from(&self, from: &str) -> Option<PendingStandingHandOff> {
        let role = self.standing_role_of_chat(from)?;
        let pending = self.pending_hand_off_of(&role.id)?;
        (pending.from_chat_id == from
            && self.hand_off_recorded(&pending.from_chat_id, &pending.to_chat_id))
        .then_some(pending)
    }

    /// Read-only precheck for a hand-off launch, run before anything is
    /// written. The launch holds the engine's launch gate and the hand-off
    /// runs on the source chat's host, so the plan stays true until
    /// [`Self::rotate_standing_role_for_chat`] commits it.
    pub fn check_standing_role_hand_off(
        &self,
        from: &str,
        to: &str,
        to_space: Option<&str>,
    ) -> Result<(), String> {
        self.plan_standing_role_hand_off(from, to, to_space)
            .map(drop)
    }

    /// Rotate the role `from` holds (if any) to `to`. Same planning rules as
    /// [`Self::check_standing_role_hand_off`], so a stale or invalid hand-off
    /// is an error here rather than a silent no-op.
    pub fn rotate_standing_role_for_chat(
        &mut self,
        from: &str,
        to: &str,
    ) -> Result<Option<StandingRole>, String> {
        match self.plan_standing_role_hand_off(from, to, None)? {
            None => Ok(None),
            Some((role, true)) => Ok(Some(role)),
            Some((role, false)) => self
                .rotate_standing_role(role.role, &role.scope_item_id, from, to)
                .map(|(role, _)| Some(role)),
        }
    }

    /// Move the role from `from` to `to`, archiving and hiding `from` in the
    /// same batch. Returns the role and whether this was a replay (the role
    /// already named `to`). Callers create `to` and move workers first; this
    /// only changes who holds the role.
    pub fn rotate_standing_role(
        &mut self,
        role: StandingRoleKind,
        scope_item_id: &str,
        from: &str,
        to: &str,
    ) -> Result<(StandingRole, bool), String> {
        let id = standing_role_id(role, scope_item_id);
        let mut record = self.standing_role_by_id(&id).ok_or("role does not exist")?;
        if record.replays_rotation(from, to) {
            return Ok((record, true));
        }
        if from == to || record.current_chat_id.as_deref() != Some(from) {
            return Err("role is not held by the source chat".into());
        }
        let source = self
            .chat(from)
            .map_err(|e| e.to_string())?
            .ok_or("source chat does not exist")?;
        self.chat(to)
            .map_err(|e| e.to_string())?
            .ok_or("destination chat does not exist")?;
        self.check_rotation_destination(&source, to, None)?;
        self.commit_rows(Self::standing_role_rotation_rows(
            &mut record,
            from,
            to,
            true,
        ))?;
        Ok((record, false))
    }

    /// Rows for the role rotation and the user's archive choice, ready to
    /// commit alongside the sidebar pin and hand-off completion marker.
    pub(super) fn user_hand_off_role_rows(
        &self,
        from: &str,
        to: &str,
        archive: Option<bool>,
    ) -> Result<Vec<RegistryUpdate>, String> {
        if let Some((mut record, false)) = self.plan_standing_role_hand_off(from, to, None)? {
            return Ok(Self::standing_role_rotation_rows(
                &mut record,
                from,
                to,
                archive.unwrap_or(true),
            ));
        }
        Ok(if archive == Some(true) {
            vec![(
                KIND_CHATS,
                from.to_owned(),
                OpKind::Update,
                fields([("archived", json!(true))]),
            )]
        } else {
            Vec::new()
        })
    }

    fn standing_role_rotation_rows(
        record: &mut StandingRole,
        from: &str,
        to: &str,
        archive: bool,
    ) -> Vec<RegistryUpdate> {
        record.current_chat_id = Some(to.to_owned());
        record.chain.push(to.to_owned());
        if record.chain.len() > MAX_CHAIN {
            let excess = record.chain.len() - MAX_CHAIN;
            record.chain.drain(..excess);
        }
        record.state = StandingRoleState::Idle;
        let mut role_set = role_fields(record);
        role_set.insert("pendingHandOff".into(), Value::Null);
        let mut source_set = fields([("standingRoleSuccessor", json!(to))]);
        if archive {
            source_set.insert("archived".into(), json!(true));
        }
        vec![
            (
                KIND_STANDING_ROLES,
                record.id.clone(),
                OpKind::Update,
                role_set,
            ),
            (KIND_CHATS, from.to_owned(), OpKind::Update, source_set),
        ]
    }

    /// Set the model a Manager's workers start on; `None` restores the
    /// default. The role must already have a record.
    pub fn set_standing_role_worker_model(
        &mut self,
        role: StandingRoleKind,
        scope_item_id: &str,
        worker_model: Option<StandingRoleModel>,
    ) -> Result<(), String> {
        if let Some(model) = &worker_model {
            StandingRoleModel::validate_chain(std::slice::from_ref(model))?;
        }
        let id = standing_role_id(role, scope_item_id);
        self.standing_role_by_id(&id).ok_or("role does not exist")?;
        self.commit_rows(vec![(
            KIND_STANDING_ROLES,
            id,
            OpKind::Update,
            fields([("workerModel", json!(worker_model))]),
        )])
    }

    pub fn set_standing_role_state(
        &mut self,
        role: StandingRoleKind,
        scope_item_id: &str,
        state: StandingRoleState,
    ) -> Result<(), String> {
        let id = standing_role_id(role, scope_item_id);
        self.standing_role_by_id(&id).ok_or("role does not exist")?;
        self.commit_rows(vec![(
            KIND_STANDING_ROLES,
            id,
            OpKind::Update,
            fields([("state", json!(state))]),
        )])
    }
}

fn role_fields(role: &StandingRole) -> BTreeMap<String, Value> {
    fields([
        ("id", json!(role.id)),
        ("role", json!(role.role)),
        ("scopeItemId", json!(role.scope_item_id)),
        ("currentChatId", json!(role.current_chat_id)),
        ("chain", json!(role.chain)),
        ("fallbackChain", json!(role.fallback_chain)),
        ("state", json!(role.state)),
        ("workerModel", json!(role.worker_model)),
        ("readOnly", json!(role.read_only)),
    ])
}
