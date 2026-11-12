//! StatusNotifierItem (SNI) and DBusMenu platform glue for Linux (GNOME, KDE, etc.).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::StreamExt;
use zbus::fdo;
use zbus::interface;
use zvariant::{ObjectPath, OwnedValue, Value};

use super::*;

pub(crate) type LayoutGroup = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

#[derive(Default, Clone)]
struct LinuxMenuState {
    view: Option<MenuView>,
    actions: Vec<MenuAction>,
    revision: u32,
}

fn str_val(s: impl Into<String>) -> OwnedValue {
    OwnedValue::try_from(Value::from(s.into())).unwrap()
}

fn bool_val(b: bool) -> OwnedValue {
    OwnedValue::from(b)
}

/// Convert straight-alpha RGBA bytes to network byte order ARGB32 (big-endian: A, R, G, B)
/// as specified by the FreeDesktop StatusNotifierItem standard.
pub(crate) fn rgba_to_argb32_be(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for chunk in rgba.chunks_exact(4) {
        let (r, g, b, a) = (chunk[0], chunk[1], chunk[2], chunk[3]);
        out.extend_from_slice(&[a, r, g, b]);
    }
    out
}

pub(crate) fn build_dbus_menu(
    items: &[Item],
) -> (Vec<(i32, HashMap<String, OwnedValue>)>, Vec<MenuAction>) {
    let mut out_items = Vec::new();
    let mut actions = Vec::new();

    for (index, item) in items.iter().enumerate() {
        let id = (index + 1) as i32;
        let mut props = HashMap::new();
        props.insert("visible".to_string(), bool_val(true));

        match item {
            Item::Title(text) => {
                props.insert("label".to_string(), str_val(text.clone()));
                props.insert("enabled".to_string(), bool_val(false));
            }
            Item::Section(text) => {
                props.insert("label".to_string(), str_val(text.clone()));
                props.insert("enabled".to_string(), bool_val(false));
            }
            Item::Chat { chat_id, text } => {
                props.insert("label".to_string(), str_val(text.text.clone()));
                props.insert("enabled".to_string(), bool_val(true));
                actions.push(MenuAction::OpenChat(chat_id.clone()));
            }
            Item::Line { text, indent } => {
                let prefix = "  ".repeat(*indent as usize);
                props.insert(
                    "label".to_string(),
                    str_val(format!("{prefix}{}", text.text)),
                );
                props.insert("enabled".to_string(), bool_val(false));
            }
            Item::Action { action, label } => {
                props.insert("label".to_string(), str_val(label.clone()));
                props.insert("enabled".to_string(), bool_val(true));
                actions.push(action.clone());
            }
            Item::Separator => {
                props.insert("type".to_string(), str_val("separator"));
                props.insert("enabled".to_string(), bool_val(false));
            }
        }

        out_items.push((id, props));
    }

    (out_items, actions)
}

pub struct StatusNotifierItemInterface {
    state: Arc<Mutex<LinuxMenuState>>,
    event_tx: UnboundedSender<MenuEvent>,
}

#[interface(name = "org.kde.StatusNotifierItem")]
impl StatusNotifierItemInterface {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    fn id(&self) -> &str {
        "mast"
    }

    #[zbus(property)]
    fn title(&self) -> &str {
        "Mast"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        let state = self.state.lock().unwrap();
        if state.view.is_some() {
            "Active"
        } else {
            "Passive"
        }
    }

    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn menu(&self) -> ObjectPath<'_> {
        ObjectPath::from_static_str_unchecked("/MenuBar")
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        let state = self.state.lock().unwrap();
        if let Some(view) = &state.view {
            let width = glyph_width(view.glyph);
            let rgba = lamp_rgba(view.glyph);
            let argb = rgba_to_argb32_be(&rgba);
            vec![(width as i32, GLYPH_PX as i32, argb)]
        } else {
            Vec::new()
        }
    }

    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<(i32, i32, Vec<u8>)>, String, String) {
        let state = self.state.lock().unwrap();
        let desc = state
            .view
            .as_ref()
            .map(|v| v.tooltip.clone())
            .unwrap_or_else(|| "Mast".to_string());
        ("mast".to_string(), Vec::new(), "Mast".to_string(), desc)
    }

    #[zbus(property)]
    fn overlay_icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn overlay_icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_movie_name(&self) -> &str {
        ""
    }

    fn activate(&self, _x: i32, _y: i32) {
        let _ = self
            .event_tx
            .unbounded_send(MenuEvent::Action(MenuAction::OpenMast));
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {}

    fn context_menu(&self, _x: i32, _y: i32) {}

    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(signal)]
    async fn new_icon(ctxt: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_tool_tip(ctxt: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_status(
        ctxt: &zbus::object_server::SignalEmitter<'_>,
        status: &str,
    ) -> zbus::Result<()>;
}

pub struct DBusMenuInterface {
    state: Arc<Mutex<LinuxMenuState>>,
    event_tx: UnboundedSender<MenuEvent>,
}

#[interface(name = "com.canonical.dbusmenu")]
impl DBusMenuInterface {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_layout(
        &self,
        parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> fdo::Result<(u32, LayoutGroup)> {
        let state = self.state.lock().unwrap();
        let revision = state.revision;

        if parent_id != 0 {
            // We have a flat menu, no submenus below parent_id > 0
            return Ok((
                revision,
                (parent_id, HashMap::new(), Vec::new()),
            ));
        }

        let mut root_props = HashMap::new();
        root_props.insert(
            "children-display".to_string(),
            str_val("submenu"),
        );

        let items = match &state.view {
            Some(view) => &view.items[..],
            None => &[][..],
        };

        let (menu_items, _) = build_dbus_menu(items);
        let mut children = Vec::with_capacity(menu_items.len());

        for (id, props) in menu_items {
            let child_tuple: LayoutGroup = (id, props, Vec::new());
            children.push(OwnedValue::try_from(Value::from(child_tuple)).unwrap());
        }

        Ok((revision, (0, root_props, children)))
    }

    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> fdo::Result<Vec<(i32, HashMap<String, OwnedValue>)>> {
        let state = self.state.lock().unwrap();
        let items = match &state.view {
            Some(view) => &view.items[..],
            None => &[][..],
        };
        let (all_items, _) = build_dbus_menu(items);
        let id_map: HashMap<i32, HashMap<String, OwnedValue>> = all_items.into_iter().collect();

        let mut out = Vec::new();
        for id in ids {
            if id == 0 {
                let mut root_props = HashMap::new();
                root_props.insert(
                    "children-display".to_string(),
                    str_val("submenu"),
                );
                out.push((0, root_props));
                continue;
            }
            if let Some(props) = id_map.get(&id) {
                let filtered = if property_names.is_empty() {
                    props.clone()
                } else {
                    props
                        .iter()
                        .filter(|(k, _)| property_names.contains(k))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                };
                out.push((id, filtered));
            }
        }
        Ok(out)
    }

    fn get_property(&self, id: i32, name: &str) -> fdo::Result<OwnedValue> {
        let state = self.state.lock().unwrap();
        if id == 0 && name == "children-display" {
            return Ok(str_val("submenu"));
        }
        let items = match &state.view {
            Some(view) => &view.items[..],
            None => &[][..],
        };
        let (all_items, _) = build_dbus_menu(items);
        for (item_id, props) in all_items {
            if item_id == id {
                if let Some(val) = props.get(name) {
                    return Ok(val.clone());
                }
            }
        }
        Err(fdo::Error::UnknownProperty(name.to_string()))
    }

    fn event(&self, id: i32, event_id: &str, _data: Value<'_>, _timestamp: u32) -> fdo::Result<()> {
        if event_id == "clicked" {
            let state = self.state.lock().unwrap();
            let action_index = (id - 1) as usize;
            if let Some(action) = state.actions.get(action_index) {
                let _ = self.event_tx.unbounded_send(MenuEvent::Action(action.clone()));
            }
        }
        Ok(())
    }

    fn about_to_show(&self, _id: i32) -> fdo::Result<bool> {
        let _ = self.event_tx.unbounded_send(MenuEvent::WillOpen);
        Ok(false)
    }

    #[zbus(signal)]
    async fn layout_updated(
        ctxt: &zbus::object_server::SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}

static EVENT_RX: Mutex<Option<UnboundedReceiver<MenuEvent>>> = Mutex::new(None);
static GLOBAL_HANDLE: OnceLock<LinuxTrayHandle> = OnceLock::new();

struct LinuxTrayHandle {
    state: Arc<Mutex<LinuxMenuState>>,
    update_tx: UnboundedSender<()>,
}

fn init_linux_tray() -> (&'static LinuxTrayHandle, UnboundedReceiver<MenuEvent>) {
    let (event_tx, event_rx) = unbounded();
    let (update_tx, update_rx) = unbounded();
    let state = Arc::new(Mutex::new(LinuxMenuState::default()));

    let handle_state = state.clone();
    let handle = LinuxTrayHandle {
        state: handle_state,
        update_tx,
    };

    let worker_state = state.clone();
    let worker_tx = event_tx.clone();

    std::thread::Builder::new()
        .name("mast-tray-sni".into())
        .spawn(move || {
            async_io::block_on(run_tray_worker(worker_state, worker_tx, update_rx));
        })
        .expect("failed to spawn mast tray dbus thread");

    let handle_ref = GLOBAL_HANDLE.get_or_init(|| handle);
    (handle_ref, event_rx)
}

async fn run_tray_worker(
    state: Arc<Mutex<LinuxMenuState>>,
    event_tx: UnboundedSender<MenuEvent>,
    mut update_rx: UnboundedReceiver<()>,
) {
    let pid = std::process::id();
    let bus_name = format!("org.kde.StatusNotifierItem-{pid}-1");

    let sni_iface = StatusNotifierItemInterface {
        state: state.clone(),
        event_tx: event_tx.clone(),
    };
    let menu_iface = DBusMenuInterface {
        state: state.clone(),
        event_tx: event_tx.clone(),
    };

    let conn_builder = match zbus::connection::Builder::session() {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(error = %err, "failed to build session bus connection for tray");
            return;
        }
    };

    let connection = match conn_builder
        .name(bus_name.as_str())
        .map_err(|e| e.to_string())
        .and_then(|b| b.serve_at("/StatusNotifierItem", sni_iface).map_err(|e| e.to_string()))
        .and_then(|b| b.serve_at("/MenuBar", menu_iface).map_err(|e| e.to_string()))
    {
        Ok(builder) => match builder.build().await {
            Ok(conn) => conn,
            Err(err) => {
                tracing::warn!(error = %err, "failed to establish dbus connection for tray");
                return;
            }
        },
        Err(err) => {
            tracing::warn!(error = %err, "failed to setup dbus interfaces for tray");
            return;
        }
    };

    let update_task = async {
        while let Some(()) = update_rx.next().await {
            let revision = {
                let s = state.lock().unwrap();
                s.revision
            };

            // Emit NewIcon and NewToolTip on StatusNotifierItem
            if let Ok(emitter) = zbus::object_server::SignalEmitter::new(
                &connection,
                ObjectPath::from_static_str_unchecked("/StatusNotifierItem"),
            ) {
                let _ = StatusNotifierItemInterface::new_icon(&emitter).await;
                let _ = StatusNotifierItemInterface::new_tool_tip(&emitter).await;
            }

            // Emit LayoutUpdated on DBusMenu
            if let Ok(emitter) = zbus::object_server::SignalEmitter::new(
                &connection,
                ObjectPath::from_static_str_unchecked("/MenuBar"),
            ) {
                let _ = DBusMenuInterface::layout_updated(&emitter, revision, 0).await;
            }
        }
    };

    let watcher_task = async {
        register_with_watcher(&connection, &bus_name).await;

        if let Ok(dbus_proxy) = fdo::DBusProxy::new(&connection).await {
            if let Ok(mut stream) = dbus_proxy.receive_name_owner_changed().await {
                while let Some(signal) = stream.next().await {
                    if let Ok(args) = signal.args() {
                        if args.name == "org.kde.StatusNotifierWatcher" && args.new_owner.is_some() {
                            register_with_watcher(&connection, &bus_name).await;
                        }
                    }
                }
            }
        }
    };

    futures::future::join(update_task, watcher_task).await;
}

async fn register_with_watcher(conn: &zbus::Connection, service: &str) {
    let result = conn
        .call_method(
            Some("org.kde.StatusNotifierWatcher"),
            "/StatusNotifierWatcher",
            Some("org.kde.StatusNotifierWatcher"),
            "RegisterStatusNotifierItem",
            &(service),
        )
        .await;

    match result {
        Ok(_) => {
            tracing::info!(service, "registered StatusNotifierItem with watcher");
        }
        Err(err) => {
            tracing::debug!(error = %err, "StatusNotifierWatcher not available (extension may not be active)");
        }
    }
}

pub fn apply(view: Option<&MenuView>) {
    let handle = GLOBAL_HANDLE.get_or_init(|| {
        let (handle, rx) = init_linux_tray();
        let mut event_slot = EVENT_RX.lock().unwrap();
        *event_slot = Some(rx);
        LinuxTrayHandle {
            state: handle.state.clone(),
            update_tx: handle.update_tx.clone(),
        }
    });

    {
        let mut state = handle.state.lock().unwrap();
        state.view = view.cloned();
        if let Some(v) = view {
            let (_, actions) = build_dbus_menu(&v.items);
            state.actions = actions;
        } else {
            state.actions.clear();
        }
        state.revision = state.revision.wrapping_add(1);
    }

    let _ = handle.update_tx.unbounded_send(());
}

pub fn take_events() -> Option<UnboundedReceiver<MenuEvent>> {
    // If tray hasn't been initialized yet, initialize it
    if GLOBAL_HANDLE.get().is_none() {
        let (_handle, rx) = init_linux_tray();
        return Some(rx);
    }

    let mut slot = EVENT_RX.lock().unwrap();
    slot.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rgba_to_argb32_be() {
        let rgba = [10, 20, 30, 255, 100, 150, 200, 128];
        let argb = rgba_to_argb32_be(&rgba);
        assert_eq!(argb, vec![255, 10, 20, 30, 128, 100, 150, 200]);
    }

    #[test]
    fn test_build_dbus_menu_items() {
        let items = vec![
            Item::Title("Mast Title".to_string()),
            Item::Separator,
            Item::Chat {
                chat_id: "chat-123".to_string(),
                text: Styled::plain("Chat 1"),
            },
            Item::Action {
                action: MenuAction::OpenMast,
                label: "Open Mast".to_string(),
            },
        ];

        let (dbus_items, actions) = build_dbus_menu(&items);
        assert_eq!(dbus_items.len(), 4);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0], MenuAction::OpenChat("chat-123".to_string()));
        assert_eq!(actions[1], MenuAction::OpenMast);

        // Title should be disabled
        assert_eq!(dbus_items[0].1.get("enabled").unwrap(), &bool_val(false));
        // Separator has type separator
        assert_eq!(
            dbus_items[1].1.get("type").unwrap(),
            &str_val("separator")
        );
        // Chat should be enabled
        assert_eq!(dbus_items[2].1.get("enabled").unwrap(), &bool_val(true));
        // Action should be enabled
        assert_eq!(dbus_items[3].1.get("enabled").unwrap(), &bool_val(true));
    }
}
