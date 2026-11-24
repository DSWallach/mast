//! Per-section metadata and one placement register per session. Concurrent moves
//! can never place a session in two sections, or in both a section and Pinned.
use super::*;
use mast_proto::{
    SidebarSection, SidebarSectionChange, pin_order_key_between, valid_pin_order_key,
};

const SECTIONS: &str = "sidebarSections";
const SCHEDULED_ORDER: &str = "sidebarScheduledOrder";
pub(super) const LOCATIONS: &str = "sidebarLocations";

impl RegistryDoc {
    pub(super) fn observe_sidebar_row(&mut self, kind: &str, id: &str) {
        if let Some(clock) = self
            .overlay_row(kind, id)
            .and_then(|r| r.max_clock().map(str::to_owned))
        {
            self.observe_clock(&clock);
        }
    }

    fn sidebar_location(&self, id: &str, pin: Option<&RegistryRow>) -> String {
        let location = self.overlay_row(LOCATIONS, id);
        // Older apps write only pinned. Honor a later legacy toggle while
        // keeping all new section/pin moves in one shared placement register.
        let location_clock = location.as_ref().and_then(|r| r.clocks.get("location"));
        let pin_clock = pin.and_then(|r| r.clocks.get("pinned"));
        if location_clock.is_some() && location_clock >= pin_clock {
            return location
                .and_then(|r| {
                    r.fields
                        .get("location")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default();
        }
        if pin
            .and_then(|r| r.fields.get("pinned"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            "pinned".into()
        } else {
            String::new()
        }
    }

    pub(super) fn sidebar_location_is_pinned(&self, id: &str, pin: &RegistryRow) -> bool {
        self.sidebar_location(id, Some(pin)) == "pinned"
    }

    pub(super) fn write_sidebar_location(&mut self, id: &str, location: &str) {
        self.observe_sidebar_row(LOCATIONS, id);
        self.observe_sidebar_row(KIND_SIDEBAR_PINS, id);
        self.write(
            LOCATIONS,
            id,
            OpKind::Upsert,
            fields([("location", json!(location))]),
        );
    }

    pub(super) fn sidebar_sections(&self) -> Vec<SidebarSection> {
        let mut rows = self.overlay_rows(SECTIONS);
        rows.retain(|row| row.fields.get("deleted").and_then(Value::as_bool) != Some(true));
        rows.sort_by(|a, b| {
            a.fields
                .get("createdAt")
                .and_then(Value::as_str)
                .cmp(&b.fields.get("createdAt").and_then(Value::as_str))
                .then(a.id.cmp(&b.id))
        });
        let locations = self.overlay_rows(LOCATIONS);
        rows.into_iter()
            .filter_map(|row| {
                let mut session_ids: Vec<_> = locations
                    .iter()
                    .filter(|r| {
                        self.sidebar_location(
                            &r.id,
                            self.overlay_row(KIND_SIDEBAR_PINS, &r.id).as_ref(),
                        ) == row.id
                    })
                    .map(|r| r.id.clone())
                    .collect();
                session_ids.sort();
                Some(SidebarSection {
                    name: row.fields.get("name")?.as_str()?.into(),
                    collapsed: row
                        .fields
                        .get("collapsed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    id: row.id,
                    session_ids,
                })
            })
            .collect()
    }

    /// Scheduled task IDs the user has placed, as (id, order key) in order. One
    /// row per task, so concurrent moves of different tasks both survive.
    pub(super) fn ordered_scheduled_tasks(&self) -> Vec<(String, String)> {
        let mut rows: Vec<_> = self
            .overlay_rows(SCHEDULED_ORDER)
            .into_iter()
            .filter_map(|row| {
                let key = row.fields.get("orderKey")?.as_str()?;
                valid_pin_order_key(key).then(|| (row.id, key.to_owned()))
            })
            .collect();
        rows.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        rows
    }

    pub(super) fn scheduled_task_order(&self) -> Vec<String> {
        self.ordered_scheduled_tasks()
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    /// Key the moved task, and any task in `order` not yet placed, so the
    /// stored keys read back as `order`. Placed tasks that already agree keep
    /// their keys, so a concurrent move of another task is left alone.
    fn move_scheduled_tasks(&mut self, moved: &str, order: &[String]) -> Result<(), DocError> {
        self.initialize_sidebar_pins();
        let keys: std::collections::HashMap<_, _> =
            self.ordered_scheduled_tasks().into_iter().collect();
        let mut lower: Option<String> = None;
        let mut ops = Vec::new();
        for (i, id) in order.iter().enumerate() {
            let above = |key: &&String| lower.as_ref().is_none_or(|l| *key > l);
            if id != moved
                && let Some(key) = keys.get(id).filter(above)
            {
                lower = Some(key.clone());
                continue;
            }
            let upper = order[i + 1..]
                .iter()
                .filter(|next| next.as_str() != moved)
                .filter_map(|next| keys.get(next))
                .find(above);
            self.observe_sidebar_row(SCHEDULED_ORDER, id);
            let hlc = self.next_hlc();
            let key = pin_order_key_between(lower.as_deref(), upper.map(String::as_str), &hlc)
                .map_err(|e| DocError::Schema(e.into()))?;
            ops.push(RowOp {
                kind: SCHEDULED_ORDER.into(),
                id: id.clone(),
                op: OpKind::Upsert,
                set: Some(fields([("orderKey", json!(key))])),
                hlc,
                clocks: None,
            });
            lower = Some(key);
        }
        if !ops.is_empty() {
            self.enqueue_ops(ops);
        }
        Ok(())
    }

    fn live_sidebar_section(&self, id: &str) -> bool {
        self.overlay_row(SECTIONS, id)
            .is_some_and(|r| r.fields.get("deleted").and_then(Value::as_bool) != Some(true))
    }

    pub(super) fn change_sidebar_section(
        &mut self,
        change: &SidebarSectionChange,
    ) -> Result<(), DocError> {
        use SidebarSectionChange::*;
        let valid_id = |id: &str| {
            !id.is_empty()
                && id.len() <= 256
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.:@/-".contains(&b))
        };
        let valid_section_id = |id: &str| valid_id(id) && id != "pinned";
        let valid_name = |name: &str| !name.trim().is_empty() && name.chars().count() <= 120;
        match change {
            Create { id, name } | Rename { id, name }
                if !valid_section_id(id) || !valid_name(name) =>
            {
                return Err(DocError::Schema("Invalid section name or ID".into()));
            }
            Collapse { id, .. } | Delete { id } if !valid_section_id(id) => {
                return Err(DocError::Schema("Invalid section ID".into()));
            }
            Assign {
                session_id,
                section_id,
            } if !valid_id(session_id)
                || section_id.as_ref().is_some_and(|id| !valid_section_id(id)) =>
            {
                return Err(DocError::Schema("Invalid section membership".into()));
            }
            Import { sections }
                if sections.iter().any(|s| {
                    !valid_section_id(&s.id)
                        || !valid_name(&s.name)
                        || s.session_ids.iter().any(|id| !valid_id(id))
                }) =>
            {
                return Err(DocError::Schema("Invalid section import".into()));
            }
            MoveScheduled { task_id, order }
                if !valid_id(task_id)
                    || !order.contains(task_id)
                    || order.len() > 500
                    || order.iter().any(|id| !valid_id(id)) =>
            {
                return Err(DocError::Schema("Invalid scheduled task ID".into()));
            }
            _ => {}
        }
        self.initialize_sidebar_pins();
        match change {
            Create { id, name } => {
                // Existing IDs, including deleted sections, are never resurrected.
                if self.overlay_row(SECTIONS, id).is_none() {
                    let created = self.next_hlc();
                    self.write(
                        SECTIONS,
                        id,
                        OpKind::Upsert,
                        fields([
                            ("name", json!(name)),
                            ("collapsed", json!(false)),
                            ("deleted", json!(false)),
                            ("createdAt", json!(created)),
                        ]),
                    );
                }
            }
            Rename { id, name } => {
                if self.live_sidebar_section(id) {
                    self.observe_sidebar_row(SECTIONS, id);
                    self.write(
                        SECTIONS,
                        id,
                        OpKind::Upsert,
                        fields([("name", json!(name))]),
                    );
                }
            }
            Collapse { id, collapsed } => {
                if self.live_sidebar_section(id) {
                    self.observe_sidebar_row(SECTIONS, id);
                    self.write(
                        SECTIONS,
                        id,
                        OpKind::Upsert,
                        fields([("collapsed", json!(collapsed))]),
                    );
                }
            }
            Delete { id } => {
                self.observe_sidebar_row(SECTIONS, id);
                self.write(
                    SECTIONS,
                    id,
                    OpKind::Upsert,
                    fields([("deleted", json!(true))]),
                );
            }
            Assign {
                session_id,
                section_id,
            } => {
                if section_id
                    .as_ref()
                    .is_some_and(|id| !self.live_sidebar_section(id))
                {
                    return Err(DocError::Schema("Section no longer exists".into()));
                }
                if self.overlay_row(KIND_CHATS, session_id).is_none() {
                    return Err(DocError::Schema("Session no longer exists".into()));
                }
                // Keep legacy clients from displaying section members as pins.
                self.observe_sidebar_row(KIND_SIDEBAR_PINS, session_id);
                self.write(
                    KIND_SIDEBAR_PINS,
                    session_id,
                    OpKind::Upsert,
                    fields([("pinned", json!(false))]),
                );
                self.write_sidebar_location(session_id, section_id.as_deref().unwrap_or(""));
                if let Some(id) = section_id {
                    self.change_sidebar_section(&Collapse {
                        id: id.clone(),
                        collapsed: false,
                    })?;
                }
            }
            Import { sections } => {
                for section in sections {
                    // Replay and migration from another device must not undo a
                    // rename, deletion, collapse, or explicit session move.
                    if self.overlay_row(SECTIONS, &section.id).is_some() {
                        continue;
                    }
                    self.change_sidebar_section(&Create {
                        id: section.id.clone(),
                        name: section.name.clone(),
                    })?;
                    for id in &section.session_ids {
                        if self.overlay_row(LOCATIONS, id).is_none()
                            && !self.ordered_sidebar_pins().iter().any(|(pin, _)| pin == id)
                            && self.overlay_row(KIND_CHATS, id).is_some()
                        {
                            self.write_sidebar_location(id, &section.id);
                        }
                    }
                    self.change_sidebar_section(&Collapse {
                        id: section.id.clone(),
                        collapsed: section.collapsed,
                    })?;
                }
            }
            MoveScheduled { task_id, order } => self.move_scheduled_tasks(task_id, order)?,
        }
        Ok(())
    }
}
