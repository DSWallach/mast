use super::*;

#[gpui::test]
fn devices_locations_folders_and_back_clear_stale_state(cx: &mut gpui::TestAppContext) {
    let data = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let shell = cx.new(|cx| {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.devices = serde_json::from_value(serde_json::json!([
                {"id":"local","name":"Studio","platform":"macos","lastSeenAt":null},
                {"id":"remote","name":"Server","platform":"linux","lastSeenAt":null}
            ]))
            .unwrap();
            state
        });
        Shell::new(
            state,
            EngineBootConfig {
                data_dir: data.path().into(),
                ipc_port: 0,
                edge_url: String::new(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: mast_proto::HarnessId::Mock,
            },
            cx,
        )
    });
    shell.update(cx, |shell, cx| {
        shell.open_add_space(cx);
        assert_eq!(shell.add_space.as_ref().unwrap().step, ProjectStep::Devices);
        assert!(shell.add_space.as_ref().unwrap().device.is_none());
        let search = shell.add_space.as_ref().unwrap().search.clone();
        search.update(cx, |input, cx| input.set_text("server", cx));
        assert_eq!(shell.add_space_devices(cx).len(), 1);
        shell.add_space_open_active(cx);
        let flow = shell.add_space.as_mut().unwrap();
        assert_eq!(flow.step, ProjectStep::Locations);
        assert_eq!(flow.device.as_ref().unwrap().id, "remote");
        assert!(flow.search.read(cx).is_empty());
        flow.drives = Loadable::Ready(vec![DriveEntry {
            name: "Projects".into(),
            path: "/projects".into(),
        }]);
        search.update(cx, |input, cx| input.set_text("projects", cx));
        shell.add_space_open_active(cx);
        let flow = shell.add_space.as_mut().unwrap();
        assert_eq!(flow.step, ProjectStep::Folders);
        assert_eq!(flow.browser_path.as_deref(), Some("/projects"));
        flow.browser = Loadable::Ready(FolderListing {
            path: "/projects".into(),
            entries: Vec::new(),
            truncated: false,
        });
        shell.add_space_go_up(cx);
        assert_eq!(
            shell.add_space.as_ref().unwrap().step,
            ProjectStep::Locations
        );
        assert!(shell.add_space.as_ref().unwrap().browser.ready().is_none());
        shell.add_space_go_up(cx);
        let flow = shell.add_space.as_ref().unwrap();
        assert_eq!(flow.step, ProjectStep::Devices);
        assert!(flow.device.is_none());
        assert!(flow.drives.ready().is_none());
        assert!(flow.search.read(cx).is_empty());
        // Slash navigation only applies to folders, never device search.
        search.update(cx, |input, cx| input.set_text("/projects/", cx));
        assert!(!shell.add_space_slash_descend(cx));
    });
}

/// Folder-pick mode: the same browser on a fixed device; submit hands
/// the open folder to the requester instead of creating a Space, and
/// the device can't be changed.
#[gpui::test]
fn folder_pick_hands_the_open_folder_to_the_requester(cx: &mut gpui::TestAppContext) {
    let data = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let shell = cx.new(|cx| {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.devices = serde_json::from_value(serde_json::json!([
                {"id":"remote","name":"Server","platform":"linux","lastSeenAt":null}
            ]))
            .unwrap();
            state
        });
        Shell::new(
            state,
            EngineBootConfig {
                data_dir: data.path().into(),
                ipc_port: 0,
                edge_url: String::new(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: mast_proto::HarnessId::Mock,
            },
            cx,
        )
    });
    let picked = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
    let sink = picked.clone();
    shell.update(cx, |shell, cx| {
        shell.open_folder_pick(
            crate::work_outline::execution::ExecutionFolderPick {
                title: "Add repository".into(),
                confirm_label: "Add repository".into(),
                device_id: "remote".into(),
                start_path: Some("/srv/work".into()),
                reject_home_directory: false,
                on_pick: Box::new(move |path, _| *sink.borrow_mut() = Some(path)),
            },
            cx,
        );
        let flow = shell.add_space.as_mut().unwrap();
        assert_eq!(flow.step, ProjectStep::Folders);
        assert_eq!(flow.device.as_ref().unwrap().id, "remote");
        assert_eq!(flow.browser_path.as_deref(), Some("/srv/work"));
        flow.browser = Loadable::Ready(FolderListing {
            path: "/srv/work/ui".into(),
            entries: Vec::new(),
            truncated: false,
        });
        // The device is fixed.
        shell.add_space_back_to(ProjectStep::Devices, cx);
        assert_eq!(shell.add_space.as_ref().unwrap().step, ProjectStep::Folders);
        let spaces_before = shell.state.read(cx).spaces.len();
        shell.submit_add_space(cx);
        assert!(shell.add_space.is_none());
        assert_eq!(
            shell.state.read(cx).spaces.len(),
            spaces_before,
            "no Space created"
        );
    });
    assert_eq!(picked.borrow().as_deref(), Some("/srv/work/ui"));
}

/// An execution-root pick never confirms the device's home directory
/// (validation would reject it with `homeDirectoryRoot`); a project
/// folder inside it is fine.
#[gpui::test]
fn execution_root_pick_refuses_the_home_directory(cx: &mut gpui::TestAppContext) {
    let data = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let shell = cx.new(|cx| {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.devices = serde_json::from_value(serde_json::json!([
                {"id":"remote","name":"Server","platform":"linux","lastSeenAt":null}
            ]))
            .unwrap();
            state
        });
        Shell::new(
            state,
            EngineBootConfig {
                data_dir: data.path().into(),
                ipc_port: 0,
                edge_url: String::new(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: mast_proto::HarnessId::Mock,
            },
            cx,
        )
    });
    let picked = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
    let sink = picked.clone();
    shell.update(cx, |shell, cx| {
        shell.open_folder_pick(
            crate::work_outline::execution::ExecutionFolderPick {
                title: "Execution root".into(),
                confirm_label: "Use this folder".into(),
                device_id: "remote".into(),
                start_path: None,
                reject_home_directory: true,
                on_pick: Box::new(move |path, _| *sink.borrow_mut() = Some(path)),
            },
            cx,
        );
        let flow = shell.add_space.as_mut().unwrap();
        // The browser resolved the device's home.
        flow.home = Some("/srv/me".into());
        flow.browser = Loadable::Ready(FolderListing {
            path: "/srv/me".into(),
            entries: Vec::new(),
            truncated: false,
        });
        assert!(flow.pick_refusal().is_some());
        shell.submit_add_space(cx);
        let flow = shell.add_space.as_mut().expect("the picker stays open");
        assert_eq!(
            flow.error.as_deref(),
            Some(crate::work_outline::execution::HOME_ROOT_REFUSAL)
        );
        // A project folder inside home is accepted.
        flow.browser = Loadable::Ready(FolderListing {
            path: "/srv/me/app".into(),
            entries: Vec::new(),
            truncated: false,
        });
        assert!(flow.pick_refusal().is_none());
    });
    assert!(picked.borrow().is_none());
    shell.update(cx, |shell, cx| shell.submit_add_space(cx));
    assert_eq!(picked.borrow().as_deref(), Some("/srv/me/app"));
}
