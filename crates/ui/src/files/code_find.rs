//! Find within the active code document.

use super::FilesSurface;
use crate::composer::{ComposerInput, ComposerInputEvent};
use gpui::{
    AnyElement, App, Context, Entity, Focusable as _, IntoElement as _, KeyBinding, SharedString,
    Subscription, Task, Window, div, prelude::*,
};
use std::{ops::Range, time::Duration};

gpui::actions!(file_find, [Open, Next, Previous]);

pub(crate) fn bind_keys(cx: &mut App) {
    let key = crate::settings::platform_combo;
    cx.bind_keys([
        KeyBinding::new(&key("mod-f"), Open, Some("Files > Input")),
        KeyBinding::new(&key("mod-f"), Open, Some("Files > FileFind")),
        KeyBinding::new(&key("mod-f"), Open, Some("Files > FilePreview")),
        KeyBinding::new(&key("mod-g"), Next, Some("Files > Input")),
        KeyBinding::new(&key("mod-g"), Next, Some("Files > FileFind")),
        KeyBinding::new(&key("mod-g"), Next, Some("Files > FilePreview")),
        KeyBinding::new(&key("mod-shift-g"), Previous, Some("Files > Input")),
        KeyBinding::new(&key("mod-shift-g"), Previous, Some("Files > FileFind")),
        KeyBinding::new(&key("mod-shift-g"), Previous, Some("Files > FilePreview")),
    ]);
}

#[derive(Clone)]
struct Match {
    range: Range<usize>,
    line: usize,
}

pub(super) struct CodeFind {
    input: Entity<ComposerInput>,
    _subscription: Subscription,
    task: Option<Task<()>>,
    matches: Vec<Match>,
    current: usize,
    case_sensitive: bool,
    generation: u64,
    focus_pending: bool,
    status: String,
    truncated: bool,
    source: Option<(String, u64, u64, Option<String>, SharedString)>,
}

impl FilesSurface {
    pub(super) fn reset_code_find_document(&mut self) {
        if let Some(find) = self.code_find.as_mut() {
            find.generation = find.generation.wrapping_add(1);
            find.task = None;
            find.matches.clear();
            find.current = usize::MAX;
            find.source = None;
            find.status.clear();
        }
    }

    pub(super) fn open_code_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.code_find.is_none() {
            let input = cx.new(|cx| ComposerInput::new("Find in file…", cx).with_single_line());
            let subscription =
                cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                    ComposerInputEvent::Edited => this.update_code_find(cx),
                    ComposerInputEvent::Submitted => this.move_code_find(true, window, cx),
                    ComposerInputEvent::ModifiedSubmitted => this.move_code_find(false, window, cx),
                    ComposerInputEvent::MentionDismiss => {
                        this.code_find = None;
                        this.focus_editor(window, cx);
                        cx.notify();
                    }
                    _ => {}
                });
            self.code_find = Some(CodeFind {
                input,
                _subscription: subscription,
                task: None,
                matches: Vec::new(),
                current: usize::MAX,
                case_sensitive: false,
                generation: 0,
                focus_pending: true,
                status: String::new(),
                truncated: false,
                source: None,
            });
        } else if let Some(find) = self.code_find.as_mut() {
            find.focus_pending = true;
        }
        cx.notify();
    }

    pub(super) fn update_code_find(&mut self, cx: &mut Context<Self>) {
        self.update_code_find_with_source(None, cx);
    }

    pub(super) fn update_code_find_with_source(
        &mut self,
        supplied: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.preview.active.clone() else {
            return;
        };
        let Some(document) = self.preview.documents.get(&path) else {
            return;
        };
        let revision = document.revision;
        let generation = document.generation;
        let content_hash = document.loaded_hash.clone();
        let Some(find) = self.code_find.as_mut() else {
            return;
        };
        let source = supplied
            .or_else(|| {
                find.source
                    .as_ref()
                    .filter(
                        |(cached_path, cached_generation, cached_revision, cached_hash, _)| {
                            cached_path == &path
                                && *cached_generation == generation
                                && *cached_revision == revision
                                && cached_hash == &content_hash
                        },
                    )
                    .map(|(_, _, _, _, source)| source.clone())
            })
            .or_else(|| {
                document
                    .editor
                    .as_ref()
                    .map(|editor| editor.read(cx).value())
            })
            .or_else(|| {
                document.file.as_ref().and_then(|file| {
                    file.text
                        .as_ref()
                        .map(|text| SharedString::from(text.clone()))
                })
            });
        let Some(source) = source else {
            return;
        };
        find.source = Some((
            path.clone(),
            generation,
            revision,
            content_hash.clone(),
            source.clone(),
        ));
        find.generation = find.generation.wrapping_add(1);
        let request = find.generation;
        find.task = None;
        find.matches.clear();
        find.current = usize::MAX;
        let query = find.input.read(cx).text().to_owned();
        let case_sensitive = find.case_sensitive;
        if query.is_empty() {
            find.status.clear();
            cx.notify();
            return;
        }
        if query.chars().count() > 256 {
            find.status = "Search is too long".into();
            cx.notify();
            return;
        }
        find.status = "Searching…".into();
        find.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let (matches, truncated) = cx
                .background_executor()
                .spawn(async move { find_matches(&source, &query, case_sensitive) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.preview.active.as_deref() != Some(&path)
                    || !this.preview.documents.get(&path).is_some_and(|document| {
                        document.revision == revision
                            && document.generation == generation
                            && document.loaded_hash == content_hash
                    })
                {
                    return;
                }
                let Some(find) = this
                    .code_find
                    .as_mut()
                    .filter(|find| find.generation == request)
                else {
                    return;
                };
                find.matches = matches;
                find.truncated = truncated;
                find.status = if find.matches.is_empty() {
                    "No matches".into()
                } else if truncated {
                    format!("First {} matches", find.matches.len())
                } else {
                    format!("{} matches", find.matches.len())
                };
                find.task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn move_code_find(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(find) = self.code_find.as_mut() else {
            if forward {
                window.dispatch_action(Box::new(crate::shell::GoToLine), cx);
            }
            return;
        };
        if find.matches.is_empty() {
            return;
        }
        let count = find.matches.len();
        find.current = if find.current == usize::MAX {
            if forward { 0 } else { count - 1 }
        } else if forward {
            (find.current + 1) % count
        } else {
            (find.current + count - 1) % count
        };
        find.status = if find.truncated {
            format!("{} of first {count}", find.current + 1)
        } else {
            format!("{} of {count}", find.current + 1)
        };
        let matched = find.matches[find.current].clone();
        if let Some(editor) = self
            .preview
            .active
            .as_ref()
            .and_then(|path| self.preview.documents.get(path))
            .and_then(|document| document.editor.clone())
        {
            editor.update(cx, |editor, cx| {
                editor.set_selected_range(matched.range, cx)
            });
            window.focus(&editor.focus_handle(cx), cx);
        } else {
            self.preview.list.scroll_to_reveal_item(matched.line);
        }
        cx.notify();
    }

    pub(super) fn render_code_find(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let find = self.code_find.as_mut()?;
        if std::mem::take(&mut find.focus_pending) {
            window.focus(&find.input.focus_handle(cx), cx);
        }
        let input = find.input.clone();
        let status = find.status.clone();
        let case_sensitive = find.case_sensitive;
        let theme = crate::theme::Theme::of(cx).clone();
        Some(
            crate::surface_chrome::toolbar(&theme)
                .child(
                    div()
                        .key_context("FileFind")
                        .flex_1()
                        .min_w_0()
                        .child(input),
                )
                .child(div().text_color(theme.text_muted).child(status))
                .child(
                    super::toolbar_button("code-find-case", "Match case", &theme)
                        .when(true, |el| {
                            crate::components::file_toolbar_selection(el, &theme, case_sensitive)
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(find) = this.code_find.as_mut() {
                                find.case_sensitive = !find.case_sensitive;
                            }
                            this.update_code_find(cx);
                        }))
                        .child("Aa"),
                )
                .child(
                    super::toolbar_button("code-find-previous", "Previous match", &theme)
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.move_code_find(false, window, cx)
                            }),
                        )
                        .child("↑"),
                )
                .child(
                    super::toolbar_button("code-find-next", "Next match", &theme)
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.move_code_find(true, window, cx)
                            }),
                        )
                        .child("↓"),
                )
                .child(
                    super::toolbar_button("code-find-close", "Close find", &theme)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.code_find = None;
                            this.focus_editor(window, cx);
                            cx.notify();
                        }))
                        .child("×"),
                )
                .into_any_element(),
        )
    }
}

fn find_matches(source: &str, query: &str, case_sensitive: bool) -> (Vec<Match>, bool) {
    const MAX_MATCHES: usize = 10_000;
    if query.is_empty() {
        return (Vec::new(), false);
    }
    let Ok(regex) = regex::RegexBuilder::new(&regex::escape(query))
        .case_insensitive(!case_sensitive)
        .size_limit(1024 * 1024)
        .build()
    else {
        return (Vec::new(), false);
    };
    let mut line = 0;
    let mut scanned = 0;
    let mut matches: Vec<_> = regex
        .find_iter(source)
        .take(MAX_MATCHES + 1)
        .map(|matched| {
            line += source.as_bytes()[scanned..matched.start()]
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count();
            scanned = matched.start();
            Match {
                range: matched.range(),
                line,
            }
        })
        .collect();
    let truncated = matches.len() > MAX_MATCHES;
    matches.truncate(MAX_MATCHES);
    (matches, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_case_and_unicode_with_byte_ranges_and_lines() {
        let (matches, truncated) = find_matches("Äbc\nabc ÄBC", "äbc", false);
        assert!(!truncated);
        assert_eq!(
            matches
                .iter()
                .map(|m| (m.range.clone(), m.line))
                .collect::<Vec<_>>(),
            vec![(0..4, 0), (9..13, 1)]
        );
        assert!(find_matches("Äbc", "äbc", true).0.is_empty());
        assert!(find_matches("abc", "", false).0.is_empty());
        assert!(find_matches(&"a".repeat(10_001), "a", true).1);
    }

    #[gpui::test]
    fn find_shortcut_counts_and_navigates_matches(cx: &mut gpui::TestAppContext) {
        use super::super::document::{DocumentKey, FileDocument};
        use gpui::AppContext as _;
        cx.update(|cx| {
            gpui_base::init(cx);
            crate::composer::init(cx, Default::default());
            bind_keys(cx);
            cx.set_global(crate::theme::Theme::dark());
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new(state, "chat".into(), false, 1000, 13.0, false, false, cx)
        });
        window
            .update(cx, |surface, window, cx| {
                let text = "Alpha\nalpha\n";
                let mut document = FileDocument::loading(DocumentKey {
                    chat_id: "chat".into(),
                    checkout_id: Some("checkout".into()),
                    path: "a.rs".into(),
                });
                document.set_loaded(mast_proto::WorkspaceFileText {
                    checkout_id: "checkout".into(),
                    path: "a.rs".into(),
                    text: Some(text.into()),
                    content_hash: Some("hash".into()),
                    size: text.len() as u64,
                    modified_at: None,
                    encoding: mast_proto::WorkspaceTextEncoding::Utf8,
                    line_ending: Some(mast_proto::WorkspaceLineEnding::Lf),
                    read_only_reason: None,
                    truncated: false,
                });
                let theme = crate::theme::Theme::of(cx).clone();
                let editor =
                    super::super::editor::new_file_editor(text, "a.rs", false, &theme, window, cx);
                window.focus(&editor.focus_handle(cx), cx);
                document.editor = Some(editor);
                surface.preview.active = Some("a.rs".into());
                surface.preview.documents.insert("a.rs".into(), document);
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        window
            .update(cx, |surface, window, cx| {
                let editor = surface.preview.documents["a.rs"].editor.as_ref().unwrap();
                window.focus(&editor.focus_handle(cx), cx);
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        window
            .update(cx, |surface, window, cx| {
                assert!(
                    surface.editor_is_focused(window, cx),
                    "editor must be focused before shortcut"
                )
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), &crate::settings::platform_combo("mod-f"));
        window
            .update(cx, |surface, _, _| {
                assert!(surface.code_find.is_some(), "find opens from the editor")
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        cx.simulate_input(window.into(), "alpha");
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(150));
        cx.run_until_parked();
        window
            .update(cx, |surface, _, _| {
                assert_eq!(surface.code_find.as_ref().unwrap().matches.len(), 2);
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        cx.simulate_keystrokes(window.into(), &crate::settings::platform_combo("mod-g"));
        window
            .update(cx, |surface, _, cx| {
                assert_eq!(
                    surface.preview.documents["a.rs"]
                        .editor
                        .as_ref()
                        .unwrap()
                        .read(cx)
                        .selected_range(),
                    0..5
                );
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        cx.simulate_keystrokes(window.into(), &crate::settings::platform_combo("mod-g"));
        window
            .update(cx, |surface, _, cx| {
                assert_eq!(
                    surface.preview.documents["a.rs"]
                        .editor
                        .as_ref()
                        .unwrap()
                        .read(cx)
                        .selected_range(),
                    6..11
                );
            })
            .unwrap();
    }
}
