//! Session-scoped locations for back and forward navigation.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileLocation {
    pub path: String,
    pub line: u32,
}

#[derive(Default)]
pub(super) struct FileNavigation {
    entries: Vec<FileLocation>,
    cursor: usize,
}

impl FileNavigation {
    pub fn record(&mut self, location: FileLocation) {
        if self.entries.get(self.cursor) == Some(&location) {
            return;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.cursor + 1);
        }
        self.entries.push(location);
        if self.entries.len() > 100 {
            self.entries.remove(0);
        }
        self.cursor = self.entries.len() - 1;
    }

    pub fn move_by(&mut self, forward: bool) -> Option<FileLocation> {
        if self.entries.is_empty()
            || (!forward && self.cursor == 0)
            || (forward && self.cursor + 1 >= self.entries.len())
        {
            return None;
        }
        if forward {
            self.cursor += 1;
        } else {
            self.cursor -= 1;
        }
        self.entries.get(self.cursor).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn location(path: &str, line: u32) -> FileLocation {
        FileLocation {
            path: path.into(),
            line,
        }
    }
    #[test]
    fn back_forward_and_new_jumps_discard_forward_history() {
        let mut history = FileNavigation::default();
        history.record(location("a.rs", 1));
        history.record(location("a.rs", 5));
        history.record(location("b.rs", 2));
        assert_eq!(history.move_by(false), Some(location("a.rs", 5)));
        assert_eq!(history.move_by(false), Some(location("a.rs", 1)));
        assert_eq!(history.move_by(false), None);
        assert_eq!(history.move_by(true), Some(location("a.rs", 5)));
        history.record(location("c.rs", 1));
        assert_eq!(history.move_by(true), None);
        for line in 1..=150 {
            history.record(location("a.rs", line));
        }
        assert_eq!(history.entries.len(), 100);
    }

    #[gpui::test]
    fn navigation_reopens_the_previous_file_and_line(cx: &mut gpui::TestAppContext) {
        use super::super::*;
        let directory = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            settings::init(settings::UiSettings::default(), directory.path(), cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let host = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: directory.path().into(),
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
        host.update(cx, |shell, window, cx| {
            shell.active_chat = "chat".into();
            shell.add_file_surface_at("a.rs".into(), Some(10), None, window, cx);
            shell.add_file_surface_at("b.rs".into(), Some(20), None, window, cx);
            shell.move_file_navigation(false, window, cx);
            let RightSurface::File(id) = shell.resolved_right_active(cx) else {
                panic!("file not active")
            };
            assert_eq!(shell.file_surface_paths[&id], "a.rs");
            assert_eq!(
                shell.file_surfaces[&id]
                    .read(cx)
                    .pending_location_for_test(),
                Some((10, None))
            );
            shell.move_file_navigation(true, window, cx);
            let RightSurface::File(id) = shell.resolved_right_active(cx) else {
                panic!("file not active")
            };
            assert_eq!(shell.file_surface_paths[&id], "b.rs");
            assert_eq!(
                shell.file_surfaces[&id]
                    .read(cx)
                    .pending_location_for_test(),
                Some((20, None))
            );
        })
        .unwrap();
    }
}
