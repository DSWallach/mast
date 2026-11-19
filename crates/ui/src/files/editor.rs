//! Boundary between the Files surface and the `gpui-base` editor.

use std::rc::Rc;

use gpui::{
    AnyElement, AppContext as _, Context, Entity, Focusable as _, IntoElement as _, SharedString,
    Subscription, Window,
};
use gpui_base::input::{EditorState, InputContextMenuCapabilities, InputEvent};

use super::FilesSurface;
use crate::theme::Theme;

pub(super) type FileEditorState = EditorState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EditorMenuAvailability {
    pub(super) cut: bool,
    pub(super) copy: bool,
    pub(super) paste: bool,
}

impl EditorMenuAvailability {
    fn new(capabilities: InputContextMenuCapabilities, clipboard_has_text: bool) -> Self {
        let editable = capabilities.is_editable();
        let selection = capabilities.has_selection();
        Self {
            cut: editable && selection,
            copy: selection,
            paste: editable && clipboard_has_text,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EditorContextAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// Locate the identifier at a cursor offset for keyboard and context-menu navigation.
/// The editor exposes the cursor but not its syntax tree; use its source text here.
pub(super) fn identifier_at_cursor(source: &str, offset: usize, path: &str) -> Option<std::ops::Range<usize>> {
    if offset > source.len() || !source.is_char_boundary(offset) { return None; }
    let ruby = mast_syntax::language_for_path(path) == Some(mast_syntax::LanguageId::Ruby);
    let is_identifier = |character: char| character.is_alphanumeric() || character == '_';
    let anchor = if ruby && offset > 0 && matches!(source.as_bytes()[offset - 1], b'?' | b'!') { offset - 1 } else { offset };
    let start = source[..anchor].char_indices().rev()
        .find(|(_, character)| !is_identifier(*character))
        .map_or(0, |(index, character)| index + character.len_utf8());
    let mut end = source[anchor..].char_indices()
        .find(|(_, character)| !is_identifier(*character))
        .map_or(source.len(), |(index, _)| anchor + index);
    if ruby && start < end && matches!(source.as_bytes().get(end), Some(b'?' | b'!'))
        && !(source.as_bytes()[end] == b'!' && source.as_bytes().get(end + 1) == Some(&b'=')) { end += 1; }
    (start < end).then_some(start..end)
}

/// Exclude the line-number gutter from definition-click fallback handling.
pub(super) fn pointer_in_text(editor: &FileEditorState, position: gpui::Point<gpui::Pixels>) -> bool {
    let bounds = editor.input_bounds();
    if !bounds.contains(&position) { return false; }
    let source = editor.value();
    let cursor = editor.cursor().min(source.len());
    let Some(prefix) = source.get(..cursor) else { return false; };
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let Some(line_bounds) = editor.range_to_bounds(&(line_start..line_start)) else { return false; };
    position.x >= line_bounds.left() - editor.scroll_offset().x
}

/// Creates a stable editor entity for an open workspace document.
pub(super) fn new_file_editor(
    text: impl Into<SharedString>,
    path: &str,
    soft_wrap: bool,
    theme: &Theme,
    window: &mut Window,
    cx: &mut Context<FilesSurface>,
) -> Entity<EditorState> {
    let editor = cx.new(|cx| {
        EditorState::new(window, cx)
            .language(path.to_string())
            .line_number(true)
            .folding(false)
            .soft_wrap(soft_wrap)
            .default_value(text)
    });
    let surface = cx.weak_entity();
    let menu_editor = editor.downgrade();
    editor.update(cx, |state, cx| {
        state.set_editor_style(super::editor_adapter::editor_style(theme));
        state.set_readonly(false, cx);
        state.on_context_menu(Rc::new(move |_, capabilities, position, _, cx| {
            let clipboard_has_text = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .is_some();
            let Some(menu_editor) = menu_editor.upgrade() else {
                return;
            };
            surface
                .update(cx, |surface, cx| {
                    surface.open_editor_context_menu(
                        menu_editor,
                        EditorMenuAvailability::new(capabilities, clipboard_has_text),
                        position,
                        cx,
                    );
                })
                .ok();
        }));
    });
    editor
}

pub(super) fn dispatch_context_action(
    editor: &Entity<FileEditorState>,
    action: EditorContextAction,
    window: &mut Window,
    cx: &mut Context<FilesSurface>,
) {
    let focus = editor.focus_handle(cx);
    window.focus(&focus, cx);
    let action: Box<dyn gpui::Action> = match action {
        EditorContextAction::Cut => Box::new(gpui_base::input::Cut),
        EditorContextAction::Copy => Box::new(gpui_base::input::Copy),
        EditorContextAction::Paste => Box::new(gpui_base::input::Paste),
        EditorContextAction::SelectAll => Box::new(gpui_base::input::SelectAll),
    };
    window.dispatch_action(action, cx);
}

pub(super) fn editor_element(editor: &Entity<FileEditorState>) -> AnyElement {
    gpui_base::input::Editor::new(editor).into_any_element()
}

/// Applies a clean disk update without discarding the user's editor context.
pub(super) fn replace_file_contents(
    editor: &Entity<FileEditorState>,
    text: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<FilesSurface>,
) {
    let text = text.into();
    editor.update(cx, |state, cx| {
        let selection = state.selected_range();
        let scroll_offset = state.scroll_offset();
        // Disk reconciliation establishes a new baseline, so it must not be
        // recorded as a user edit that can later be undone and autosaved.
        state.set_value(text, window, cx);
        state.set_selected_range(selection, cx);
        state.set_scroll_offset(scroll_offset, cx);
    });
}

/// Edit only the marker, using the normal atomic edit and Change event path.
/// A stale preview must never apply offsets to a different buffer revision.
pub(super) fn toggle_markdown_task(
    editor: &Entity<FileEditorState>,
    source: &str,
    task: &crate::markdown::parser::TaskMarker,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    editor.update(cx, |state, cx| {
        if !state.context_menu_capabilities().is_editable() || state.value().as_ref() != source {
            return;
        }
        let marker = source.get(task.range.clone());
        if !matches!(
            (marker, task.checked),
            (Some("[ ]"), false) | (Some("[x]" | "[X]"), true)
        ) {
            return;
        }
        let selection = state.selected_range();
        let scroll = state.scroll_offset();
        state.set_selected_range(task.range.start + 1..task.range.start + 2, cx);
        state.replace(if task.checked { " " } else { "x" }, window, cx);
        state.set_selected_range(selection, cx);
        state.set_scroll_offset(scroll, cx);
    });
}

pub(super) fn subscribe_to_changes(
    editor: &Entity<FileEditorState>,
    path: String,
    cx: &mut Context<FilesSurface>,
) -> Subscription {
    cx.subscribe(editor, move |surface, _, event, cx| {
        if matches!(event, InputEvent::Change) {
            surface.on_editor_change(&path, cx);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_at_cursor_respects_unicode_boundaries() {
        let text = "call(élève_2);";
        assert_eq!(&text[identifier_at_cursor(text, 8, "a.rs").unwrap()], "élève_2");
        assert!(identifier_at_cursor(text, 6, "a.rs").is_none());
        assert!(identifier_at_cursor(text, text.len(), "a.rs").is_none());
        for name in ["ready?", "save!"] {
            assert_eq!(&name[identifier_at_cursor(name, name.len(), "a.rb").unwrap()], name);
            assert_eq!(&name[identifier_at_cursor(name, name.len() - 1, "a.rb").unwrap()], name);
        }
        assert_eq!(&"ready?"[identifier_at_cursor("ready?", 2, "a.rs").unwrap()], "ready");
    }

    #[test]
    fn menu_availability_tracks_selection_editability_and_clipboard() {
        let editable_selection = InputContextMenuCapabilities::new().selection(true);
        assert_eq!(
            EditorMenuAvailability::new(editable_selection, true),
            EditorMenuAvailability {
                cut: true,
                copy: true,
                paste: true,
            }
        );

        let without_selection = InputContextMenuCapabilities::new();
        assert_eq!(
            EditorMenuAvailability::new(without_selection, true),
            EditorMenuAvailability {
                cut: false,
                copy: false,
                paste: true,
            }
        );
        assert!(!EditorMenuAvailability::new(without_selection, false).paste);

        let readonly_selection = InputContextMenuCapabilities::new()
            .selection(true)
            .readonly(true);
        assert_eq!(
            EditorMenuAvailability::new(readonly_selection, true),
            EditorMenuAvailability {
                cut: false,
                copy: true,
                paste: false,
            }
        );
    }
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[gpui::test]
    fn dropping_file_editor_releases_entity_with_context_menu(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new(state, "test".into(), false, 1000, 13.0, false, false, cx)
        });
        let weak = window
            .update(cx, |_, window, cx| {
                let theme = Theme::of(cx).clone();
                let editor = new_file_editor("unsaved text", "test.rs", false, &theme, window, cx);
                editor.downgrade()
            })
            .unwrap();
        cx.run_until_parked();
        cx.update(|_| {
            assert!(
                weak.upgrade().is_none(),
                "closed editor retained by its own menu callback"
            )
        });
    }
}
