use super::*;
use gpui::{AppContext, TestAppContext};

#[gpui::test]
fn appshot_only_draft_counts_as_content_and_removal_clears_it(cx: &mut TestAppContext) {
    let state = cx.new(|_| AppState::new());
    let composer = cx.new(|cx| Composer::new(state, cx));
    composer.update(cx, |composer, cx| {
        let shot = appshots::tests::shot();
        composer.stage_appshot(shot.clone(), cx);
        assert!(composer_has_content(
            "",
            composer.staged().len() + composer.staged_appshots().len(),
            0
        ));
        composer.remove_appshot(&shot.id, cx);
        assert!(composer.staged_appshots().is_empty());
        assert!(!composer_has_content(
            "",
            composer.staged().len() + composer.staged_appshots().len(),
            0
        ));
    });
}

#[gpui::test]
fn failed_send_restores_complete_appshots_without_duplicates(cx: &mut TestAppContext) {
    let state = cx.new(|_| AppState::new());
    let composer = cx.new(|cx| Composer::new(state, cx));
    composer.update(cx, |composer, cx| {
        let original = appshots::tests::shot();
        let mut fresh = original.clone();
        fresh.id = "fresh".into();
        composer.stage_appshot_for("minted".into(), original.clone(), cx);
        composer.stage_appshot(fresh, cx);
        composer.restore_failed_appshots(&[original.clone()], "minted", "");
        assert_eq!(composer.staged_appshots().len(), 2);
        assert_eq!(
            composer.staged_appshots()[0].accessibility,
            original.accessibility
        );
        assert_eq!(
            composer.staged_appshots()[0].screenshot.id,
            original.screenshot.id
        );
        assert_eq!(composer.staged_appshots()[1].id, "fresh");
        assert!(!composer.appshots.contains_key("minted"));
    });
}

#[gpui::test]
fn remote_queue_removal_recovers_both_appshot_drafts(cx: &mut TestAppContext) {
    let state = cx.new(|_| AppState::new());
    let composer = cx.new(|cx| Composer::new(state, cx));
    composer.update(cx, |composer, cx| {
        let original = appshots::tests::shot();
        let mut edited = original.clone();
        edited.id = "edited".into();
        composer.stage_appshot(edited, cx);
        composer.queue_edit_draft = Some(("original".into(), vec![], vec![original]));
        composer.editing_queued = Some("removed-row".into());
        composer
            .input
            .update(cx, |input, cx| input.set_text("edited", cx));
        composer.on_state_changed(cx);
        assert!(composer.editing_queued.is_none());
        assert_eq!(composer.staged_appshots().len(), 2);
        assert_eq!(composer.input.read(cx).text(), "original\n\nedited");
    });
}
