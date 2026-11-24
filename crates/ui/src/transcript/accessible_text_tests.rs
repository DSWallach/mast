use super::*;

#[test]
fn row_text_is_not_built_unless_a11y_is_active() {
    let kind = RowKind::User {
        source: crate::message_source::MessageSource::Human,
        text: "hello".into(),
        mentions: Arc::new(Vec::new()),
        attachments: Arc::new(Vec::new()),
        badges: Arc::new(Vec::new()),
        pending: false,
    };
    ACCESSIBLE_TEXT_BUILDS.with(|c| c.set(0));
    let _ = with_accessible_text(div().id("row"), &kind, false);
    assert_eq!(ACCESSIBLE_TEXT_BUILDS.with(|c| c.get()), 0);
    let _ = with_accessible_text(div().id("row"), &kind, true);
    assert_eq!(ACCESSIBLE_TEXT_BUILDS.with(|c| c.get()), 1);
}
