use super::*;

impl AppState {
    pub fn apply_connectivity(&mut self, connectivity: mast_proto::Connectivity) {
        self.connectivity = connectivity;
        self.connectivity_observed = true;
    }

    /// Is this chat's delivery path degraded — will a send QUEUE rather than
    /// reach its executor promptly? Locally-hosted chats are never degraded
    /// (a queued command executes on this device even fully offline). Remote
    /// chats degrade when the OS says offline, when the chat's own edge room
    /// is down, or when the host device has gone presence-dark.
    pub fn chat_delivery_degraded(&self, chat_id: &str) -> bool {
        use mast_proto::ConnectivityState as S;
        if self.connectivity.state == S::Disabled {
            return false;
        }
        let Some(chat) = self.chats.iter().find(|c| c.id == chat_id) else {
            // Unknown chat (a just-minted canvas send): only the global
            // state can speak.
            return self.connectivity.state == S::Offline;
        };
        if Some(chat.device_id.as_str()) == self.local_device_id.as_deref() {
            return false;
        }
        if self.connectivity.state == S::Offline {
            return true;
        }
        let room_down = match self
            .connectivity
            .chats
            .iter()
            .find(|c| c.chat_id == chat_id)
        {
            Some(net) => !net.connected,
            None => self.connectivity.state != S::Connected,
        };
        room_down || !self.device_online(&chat.device_id, Utc::now())
    }

    /// A send is queued: in flight AND its delivery path is degraded — the
    /// honest badge is "Queued", not a Working spinner.
    pub fn send_queued(&self, chat_id: &str, now: DateTime<Utc>) -> bool {
        self.send_pending(chat_id, now) && self.chat_delivery_degraded(chat_id)
    }

    pub fn apply_devices(&mut self, devices: Vec<Device>) -> bool {
        self.apply_devices_at(devices, Utc::now())
    }

    fn apply_devices_at(&mut self, mut devices: Vec<Device>, now: DateTime<Utc>) -> bool {
        // A local-only workspace has no remote device identity to distinguish.
        // Keep the engine's legacy sentinel out of the UI while preserving real
        // hostnames and user-assigned device names.
        if self.workspace_scope == Some(WorkspaceScope::Local)
            && let Some(local_id) = self.local_device_id.as_deref()
            && let Some(device) = devices.iter_mut().find(|device| device.id == local_id)
            && device.name == "unknown-device"
        {
            device.name = "Local".to_string();
        }
        for device in &devices {
            self.change_requests
                .clear_unsupported_on_version_change(&device.id, device.version.as_deref());
        }
        let presentation: Vec<_> = devices
            .iter()
            .map(|device| {
                let mut metadata = device.clone();
                metadata.last_seen_at = None;
                (
                    metadata,
                    crate::settings::devices::device_online(device.last_seen_at, now),
                    crate::settings::devices::format_last_seen(device.last_seen_at, now),
                )
            })
            .collect();
        let session_presence: Vec<_> = self
            .sessions
            .iter()
            .map(|session| effective_indicator(Some(session), now))
            .collect();
        let changed = self.device_presentation.as_ref() != Some(&presentation)
            || self.session_presence_presentation != session_presence;
        self.device_presentation = Some(presentation);
        self.session_presence_presentation = session_presence;
        // Freshness must advance even when the heartbeat does not redraw:
        // delivery gating and later renders still need the newest timestamp.
        self.devices = devices;
        changed
    }

    /// True when `device_id`'s engine (per its registry device row) is at
    /// least `min`. Unknown devices and unstamped versions are conservatively
    /// false — feature gates fall back to the legacy path rather than speak a
    /// protocol the peer may not understand.
    pub fn device_version_at_least(&self, device_id: &str, min: (u64, u64, u64)) -> bool {
        self.devices
            .iter()
            .find(|d| d.id == device_id)
            .and_then(|d| d.version.as_deref())
            .and_then(version_triple)
            .is_some_and(|v| v >= min)
    }

    /// Capability checks use the live EngineInfo for this device (including a
    /// localhost daemon that may not match the UI binary) and synced device
    /// rows for peers. Missing declarations are conservatively unsupported.
    pub fn device_supports(&self, device_id: &str, capability: &str) -> bool {
        if let Some(engine) = self.engine.as_ref()
            && engine.engine_info().device_id == device_id
        {
            return engine.engine_info().supports(capability);
        }
        self.devices
            .iter()
            .find(|device| device.id == device_id)
            .is_some_and(|device| device.supports(capability))
    }

    /// Whether the Chat's host can take Work Outline instructions: its device
    /// row must advertise `work-outline-instruction-envelope-v1` and, when the
    /// Chat's harness is known (its config), the per-harness token
    /// `work-outline-instruction-envelope-v1/<harness>`. Read from the synced
    /// device row for every host, the local engine included: `EngineInfo` is
    /// fixed at boot, while the row is restamped once the harness delivery
    /// probes run. An unknown device is unsupported.
    pub(crate) fn chat_host_instruction_support(&self, chat_id: &str) -> InstructionHostSupport {
        use mast_proto::capabilities::{
            WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1, work_outline_instruction_envelope_for,
        };
        let Some(chat) = self.chats.iter().find(|chat| chat.id == chat_id) else {
            return InstructionHostSupport::UnknownDevice;
        };
        let Some(device) = self.devices.iter().find(|d| d.id == chat.device_id) else {
            return InstructionHostSupport::UnknownDevice;
        };
        if !device.supports(WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1) {
            return InstructionHostSupport::MastUpgradeRequired;
        }
        match chat.config.as_ref().map(|config| config.harness) {
            Some(harness) if !device.supports(&work_outline_instruction_envelope_for(harness)) => {
                InstructionHostSupport::HarnessUnsupported(harness)
            }
            _ => InstructionHostSupport::Supported,
        }
    }

    /// Whether `device_id` is the device this app's engine runs on.
    pub(crate) fn is_local_device(&self, device_id: &str) -> bool {
        self.local_device_id.as_deref() == Some(device_id)
            || self
                .engine
                .as_ref()
                .is_some_and(|engine| engine.engine_info().device_id == device_id)
    }

    pub fn chat_host_supports(&self, chat_id: &str, capability: &str) -> bool {
        self.chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .is_some_and(|chat| self.device_supports(&chat.device_id, capability))
    }

    /// First project on the composer's picked device (falling back through
    /// the local device, then any project at all — better a cross-device
    /// project than a surprise project-less canvas). Display order.
    fn first_space_on_picked_device(&self) -> Option<String> {
        let device = self
            .selected_device
            .as_deref()
            .or(self.local_device_id.as_deref());
        let sorted = self.spaces_sorted();
        device
            .and_then(|d| sorted.iter().find(|s| s.device_id == d).copied())
            .or_else(|| sorted.first().copied())
            .map(|s| s.id.clone())
    }

}
