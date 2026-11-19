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
        self.standing_roles()
            .into_iter()
            .find(|role| role.current_chat_id.as_deref() == Some(chat_id))
    }

    /// Write rows as one checked registry batch: every op is size-gated first
    /// and nothing is enqueued unless all pass, and the ops go out together,
    /// so a second device never sees half of a role change.
    fn commit_rows(
        &mut self,
        rows: Vec<(&str, String, OpKind, BTreeMap<String, Value>)>,
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
    fn hand_off_recorded(&self, from: &str, to: &str) -> bool {
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
        if chat.archived || chat.is_standing_role_predecessor() {
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
            },
        };
        let record = StandingRole {
            current_chat_id: Some(chat_id.to_owned()),
            chain: vec![chat_id.to_owned()],
            ..record
        };
        self.commit_rows(vec![(
            KIND_STANDING_ROLES,
            id,
            OpKind::Upsert,
            role_fields(&record),
        )])?;
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

    /// Hand-offs a binary without role support completed: the successor
    /// recorded a hand-off from the chat that still holds a role, and nothing
    /// moved the role. Each is `(role chat, its sole active successor)`;
    /// ambiguous cases (several successors), hand-offs a live host is
    /// finishing (a reservation names that successor), and successors that
    /// cannot hold the role are left alone.
    pub fn stranded_standing_role_hand_offs(&self) -> Vec<(String, String)> {
        let chats = self.read_chats().unwrap_or_default();
        let mut stranded = Vec::new();
        for role in self.standing_roles() {
            let Some(current) = role.current_chat_id.as_deref() else {
                continue;
            };
            let pending_to = self.pending_hand_off_of(&role.id).map(|p| p.to_chat_id);
            let successors: Vec<&Chat> = chats
                .iter()
                .filter(|c| !c.archived && !c.is_standing_role_predecessor() && c.id != current)
                .filter(|c| self.hand_off_recorded(current, &c.id))
                .collect();
            let [successor] = successors.as_slice() else {
                continue;
            };
            if pending_to.as_deref() == Some(successor.id.as_str()) {
                continue;
            }
            if self
                .check_standing_role_hand_off(current, &successor.id, None)
                .is_ok()
            {
                stranded.push((current.to_owned(), successor.id.clone()));
            }
        }
        stranded
    }

    /// Finish the stranded hand-offs found by
    /// [`Self::stranded_standing_role_hand_offs`]. Returns how many moved.
    pub fn repair_stranded_standing_roles(&mut self) -> usize {
        let mut repaired = 0;
        for (from, to) in self.stranded_standing_role_hand_offs() {
            match self.rotate_standing_role_for_chat(&from, &to) {
                Ok(Some(_)) => repaired += 1,
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(from = %from, to = %to, %error, "standing role repair failed");
                }
            }
        }
        repaired
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
        record.current_chat_id = Some(to.to_owned());
        record.chain.push(to.to_owned());
        if record.chain.len() > MAX_CHAIN {
            let excess = record.chain.len() - MAX_CHAIN;
            record.chain.drain(..excess);
        }
        record.state = StandingRoleState::Idle;
        let mut role_set = role_fields(&record);
        role_set.insert("pendingHandOff".into(), Value::Null);
        self.commit_rows(vec![
            (KIND_STANDING_ROLES, id, OpKind::Update, role_set),
            (
                KIND_CHATS,
                from.to_owned(),
                OpKind::Update,
                fields([
                    ("standingRoleSuccessor", json!(to)),
                    ("archived", json!(true)),
                ]),
            ),
        ])?;
        Ok((record, false))
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
    ])
}
