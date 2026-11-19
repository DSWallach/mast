use super::*;
use mast_proto::{SidebarPinChange, pin_order_key_between, valid_pin_order_key};

impl RegistryDoc {
    fn user_chat_hand_off_completed(&self, from: &str, to: &str) -> bool {
        self.hand_off_recorded(from, to)
            && self.overlay_row(KIND_CHATS, to).is_some_and(|row| {
                row.fields
                    .get("userHandOffCompleted")
                    .and_then(Value::as_bool)
                    == Some(true)
            })
    }

    /// Resolve the successor for an attempted user hand-off. A request that
    /// resumed a reserved hand-off can have a different derived chat ID.
    pub fn completed_user_hand_off_to(&self, from: &str, requested: &str) -> Option<String> {
        if self.user_chat_hand_off_completed(from, requested) {
            return Some(requested.to_owned());
        }
        // Only a standing role's reserved hand-off can resume under a
        // different operation ID. Its predecessor points to the real chat.
        let successor = self.chat(from).ok().flatten()?.standing_role_successor?;
        let row = self.overlay_row(KIND_CHATS, &successor)?;
        let matches_request = row
            .fields
            .get("userHandOffRetryChatIds")
            .and_then(Value::as_array)
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(requested)));
        (matches_request && self.user_chat_hand_off_completed(from, &successor))
            .then_some(successor)
    }

    /// Move the pin in place and record completion in the same registry batch.
    /// A replay must leave later pin and archive edits alone.
    pub fn complete_user_chat_hand_off(
        &mut self,
        from: &str,
        to: &str,
        requested: &str,
        operation_id: &str,
        archive: Option<bool>,
    ) -> Result<(), String> {
        use super::sidebar_sections::LOCATIONS;

        if !self.hand_off_recorded(from, to) {
            return Err("the chat hand-off was not recorded".into());
        }
        let mut completion = fields([("userHandOffCompleted", json!(true))]);
        if to != requested {
            let mut ids = self
                .overlay_row(KIND_CHATS, to)
                .and_then(|row| {
                    row.fields
                        .get("userHandOffRetryChatIds")
                        .and_then(Value::as_array)
                        .cloned()
                })
                .unwrap_or_default();
            if !ids.iter().any(|id| id.as_str() == Some(requested)) {
                ids.push(json!(requested));
                completion.insert("userHandOffRetryChatIds".into(), json!(ids));
            }
        }
        if self.user_chat_hand_off_completed(from, to) {
            return if completion.len() == 1 {
                Ok(())
            } else {
                self.commit_rows(vec![(
                    KIND_CHATS,
                    to.to_owned(),
                    OpKind::Update,
                    completion,
                )])
            };
        }
        completion.insert("userHandOffOperationId".into(), json!(operation_id));
        completion.insert("userHandOffArchive".into(), json!(archive));
        let mut rows = self.user_hand_off_role_rows(from, to, archive)?;
        rows.push((KIND_CHATS, to.to_owned(), OpKind::Update, completion));
        if let Some((_, order_key)) = self
            .ordered_sidebar_pins()
            .into_iter()
            .find(|(id, _)| id == from)
        {
            rows.extend([
                (
                    LOCATIONS,
                    from.to_owned(),
                    OpKind::Upsert,
                    fields([("location", json!(""))]),
                ),
                (
                    LOCATIONS,
                    to.to_owned(),
                    OpKind::Upsert,
                    fields([("location", json!("pinned"))]),
                ),
                (
                    KIND_SIDEBAR_PINS,
                    from.to_owned(),
                    OpKind::Upsert,
                    fields([("pinned", json!(false))]),
                ),
                (
                    KIND_SIDEBAR_PINS,
                    to.to_owned(),
                    OpKind::Upsert,
                    fields([("pinned", json!(true)), ("orderKey", json!(order_key))]),
                ),
            ]);
        }
        self.commit_rows(rows)
    }

    pub fn sidebar_pins_initialized(&self) -> bool {
        self.overlay_row(KIND_PREFERENCES, SIDEBAR_PINS_STATE_ID)
            .is_some()
    }

    pub(super) fn ordered_sidebar_pins(&self) -> Vec<(String, String)> {
        let mut pins: Vec<_> = self
            .overlay_rows(KIND_SIDEBAR_PINS)
            .into_iter()
            .filter_map(|row| {
                if !self.sidebar_location_is_pinned(&row.id, &row) {
                    return None;
                }
                let key = row.fields.get("orderKey")?.as_str()?;
                valid_pin_order_key(key).then(|| (row.id, key.to_owned()))
            })
            .collect();
        pins.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        pins
    }

    /// Readiness survives restarts even when the list is empty. No data import.
    pub(super) fn initialize_sidebar_pins(&mut self) {
        if !self.sidebar_pins_initialized() {
            self.write(
                KIND_PREFERENCES,
                SIDEBAR_PINS_STATE_ID,
                OpKind::Upsert,
                fields([("initialized", json!(true))]),
            );
        }
    }

    pub fn change_sidebar_pin(&mut self, change: &SidebarPinChange) -> Result<(), DocError> {
        if let SidebarPinChange::Section { change } = change {
            return self.change_sidebar_section(change);
        }
        let id = change.session_id();
        if id.is_empty()
            || id.len() > 256
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:@/-".contains(&b))
        {
            return Err(DocError::Schema("Invalid pinned session ID".into()));
        }
        let current = self
            .sidebar_preferences()
            .unwrap_or_default()
            .pinned_session_ids;
        if matches!(change, SidebarPinChange::Pin { .. })
            && !current.iter().any(|v| v == id)
            && current.len() >= MAX_SIDEBAR_PINS
        {
            return Err(DocError::Schema("You can pin up to 200 sessions".into()));
        }
        if matches!(change, SidebarPinChange::Pin { .. })
            && self.overlay_row(KIND_CHATS, id).is_none()
        {
            return Err(DocError::Schema("Session no longer exists".into()));
        }
        self.initialize_sidebar_pins();
        self.observe_sidebar_row("sidebarLocations", id);
        // Observe this pin's field clocks before a causally subsequent edit,
        // including clocks from a device whose wall clock runs ahead of ours.
        if let Some(row) = self.overlay_row(KIND_SIDEBAR_PINS, id)
            && let Some(clock) = row.max_clock()
        {
            let mut parts = clock.splitn(3, '-');
            if let (Some(ms), Some(counter)) = (
                parts.next().and_then(|v| v.parse::<i64>().ok()),
                parts.next().and_then(|v| v.parse::<u32>().ok()),
            ) && (ms, counter) > (self.clock.last_ms, self.clock.counter)
            {
                self.clock.last_ms = ms;
                self.clock.counter = counter;
            }
        }
        if matches!(change, SidebarPinChange::Unpin { .. }) {
            if current.iter().any(|v| v == id) {
                self.write_sidebar_location(id, "");
            }
            self.write(
                KIND_SIDEBAR_PINS,
                id,
                OpKind::Upsert,
                fields([("pinned", json!(false))]),
            );
            return Ok(());
        }
        if matches!(change, SidebarPinChange::Move { .. }) && !current.iter().any(|v| v == id) {
            return Ok(());
        }
        let pins = self.ordered_sidebar_pins();
        let mut next = current.clone();
        change.project(&mut next);
        let index = next.iter().position(|v| v == id).unwrap();
        let key_for = |id: &String| {
            pins.iter()
                .find(|(pin, _)| pin == id)
                .map(|(_, key)| key.as_str())
        };
        let lower = index
            .checked_sub(1)
            .and_then(|i| next.get(i))
            .and_then(key_for);
        let upper = next.get(index + 1).and_then(key_for);
        let hlc = self.next_hlc();
        let key =
            pin_order_key_between(lower, upper, &hlc).map_err(|e| DocError::Schema(e.into()))?;
        let mut set = fields([("orderKey", json!(key))]);
        if matches!(change, SidebarPinChange::Pin { .. }) {
            set.insert("pinned".into(), json!(true));
            self.write_sidebar_location(id, "pinned");
        }
        self.enqueue_ops(vec![RowOp {
            kind: KIND_SIDEBAR_PINS.into(),
            id: id.into(),
            op: OpKind::Upsert,
            set: Some(set),
            hlc,
            clocks: None,
        }]);
        Ok(())
    }
}
