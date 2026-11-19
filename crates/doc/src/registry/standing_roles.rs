use super::*;
use mast_proto::{
    StandingRole, StandingRoleKind, StandingRoleModel, StandingRoleState, standing_role_id,
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

    /// Make `chat_id` the role's chat, creating the record on first use.
    /// Replays of the same adoption succeed; adopting over a different live
    /// holder is refused (rotation is the only way to change holders).
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
        if let Some(existing) = self.standing_role_by_id(&id) {
            return match existing.current_chat_id.as_deref() {
                Some(current) if current == chat_id => Ok(existing),
                Some(_) => Err("role already has a different chat; rotate it instead".into()),
                None => self.write_role_chat(existing, chat_id, &chat),
            };
        }
        let record = StandingRole {
            id,
            role,
            scope_item_id: scope_item_id.to_owned(),
            current_chat_id: None,
            chain: Vec::new(),
            fallback_chain: fallback_chain.to_vec(),
            state: StandingRoleState::Idle,
        };
        self.write_role_chat(record, chat_id, &chat)
    }

    fn write_role_chat(
        &mut self,
        mut record: StandingRole,
        chat_id: &str,
        chat: &Chat,
    ) -> Result<StandingRole, String> {
        if chat.archived || chat.is_standing_role_predecessor() {
            return Err("an archived or superseded chat cannot hold a role".into());
        }
        record.current_chat_id = Some(chat_id.to_owned());
        record.chain = vec![chat_id.to_owned()];
        self.write(
            KIND_STANDING_ROLES,
            &record.id.clone(),
            OpKind::Upsert,
            role_fields(&record),
        );
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
        self.check_rotation_destination(&source, to, to_space)?;
        Ok(Some((role, false)))
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
        self.write(
            KIND_STANDING_ROLES,
            &id,
            OpKind::Update,
            role_fields(&record),
        );
        self.write(
            KIND_CHATS,
            from,
            OpKind::Update,
            fields([
                ("standingRoleSuccessor", json!(to)),
                ("archived", json!(true)),
            ]),
        );
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
        self.write(
            KIND_STANDING_ROLES,
            &id,
            OpKind::Update,
            fields([("state", json!(state))]),
        );
        Ok(())
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
