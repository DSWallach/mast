//! Component fixtures use the same primitives as the application.
use gpui::{AnyElement, div, prelude::*, px};
use mast_ui::{
    components as c,
    theme::Theme,
    ui_style::{self, UiStyleId},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Story {
    Button,
    Toolbar,
    Chip,
    SidebarRow,
    OutlineRow,
    ComposerFooter,
    Message,
    Tool,
    Code,
    Card,
    Input,
    Question,
    Request,
}

pub const STORIES: &[(Story, &str)] = &[
    (Story::Button, "Button"),
    (Story::Toolbar, "Toolbar control"),
    (Story::Chip, "Chip"),
    (Story::SidebarRow, "Sidebar row"),
    (Story::OutlineRow, "Outline row"),
    (Story::ComposerFooter, "Composer footer"),
    (Story::Message, "Message"),
    (Story::Tool, "Tool panel"),
    (Story::Code, "Code panel"),
    (Story::Card, "Card"),
    (Story::Input, "Input"),
    (Story::Question, "Question card"),
    (Story::Request, "Required action"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Default,
    Selected,
    Disabled,
    LongText,
    Empty,
    Error,
}
pub const STATES: &[(State, &str)] = &[
    (State::Default, "Default"),
    (State::Selected, "Hover / selected"),
    (State::Disabled, "Disabled"),
    (State::LongText, "Long text"),
    (State::Empty, "Empty"),
    (State::Error, "Error"),
];

fn fixture(story: Story, state: State, theme: &Theme, id: String) -> AnyElement {
    let text = match state {
        State::LongText => "A longer example that exercises the available space in this component",
        State::Empty => "",
        State::Error => "Unable to complete the request",
        _ => "Example item",
    };
    let selected = state == State::Selected;
    let disabled = state == State::Disabled;
    let element = match story {
        Story::Button => c::button(
            id,
            text,
            if state == State::Error {
                c::ButtonVariant::Danger
            } else {
                c::ButtonVariant::Primary
            },
            c::ButtonSize::Md,
            theme,
        )
        .into_any_element(),
        Story::Toolbar => {
            c::toolbar_control(div().px(px(8.)).py(px(4.)).child(text), theme, selected)
                .into_any_element()
        }
        Story::Chip => if selected {
            c::badge_active(theme, text)
        } else {
            c::badge(theme, text)
        }
        .into_any_element(),
        Story::SidebarRow => mast_ui::stories::sidebar_row(theme, id, text, selected),
        Story::OutlineRow => {
            let row_theme = c::outline_row_theme(theme, selected);
            let row = c::outline_row(id.into(), 30., 8., theme)
                .when(selected, |row| row.bg(theme.element_active))
                .text_color(row_theme.text)
                .child(text);
            c::outline_row_selection(row, theme, selected).into_any_element()
        }
        Story::ComposerFooter => c::conversation_footer_slot(
            32.,
            1.,
            0.,
            1.,
            None,
            Some(
                c::conversation_picker_footer_row()
                    .child(text)
                    .into_any_element(),
            ),
            None,
        )
        .into_any_element(),
        Story::Message => c::conversation_user_bubble(
            theme,
            theme.identity_color(""),
            260.,
            20.,
            disabled,
            div().child(text).into_any_element(),
        ),
        Story::Tool => c::conversation_tool_panel(
            theme,
            theme.accent,
            c::conversation_tool_chip_frame(false, 36., 30., 4., 1., theme)
                .child(text)
                .into_any_element(),
        ),
        Story::Code => c::conversation_code_frame(
            id.into(),
            Some("rust"),
            vec![],
            div().p(px(8.)).child(text).into_any_element(),
            theme,
        )
        .into_any_element(),
        Story::Card => c::section_card(theme)
            .p(px(12.))
            .child(text)
            .into_any_element(),
        Story::Input => {
            c::text_input(theme, div().child(text).into_any_element()).into_any_element()
        }
        Story::Question => {
            c::conversation_question_card(theme, id.clone().into(), theme.accent, disabled)
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .child(c::conversation_question_id(theme, "Q-1".into()))
                        .child(c::conversation_question_pill(
                            theme,
                            "Open",
                            theme.accent,
                            true,
                        )),
                )
                .child(text)
                .child(c::conversation_question_chip(
                    "Which layout?".into(),
                    selected,
                    theme,
                ))
                .child(
                    c::conversation_answer_input(theme, format!("{id}-input")).child("Your answer"),
                )
                .child(c::conversation_answer_button(
                    format!("{id}-answer").into(),
                    format!("{id}-answer"),
                    "Answer",
                    true,
                    theme,
                ))
                .into_any_element()
        }
        Story::Request => c::outline_required_action_row(id.clone().into(), theme)
            .flex_col()
            .gap(px(6.))
            .p(px(8.))
            .child(c::outline_needs_heading(
                "Blocking",
                2,
                theme.warning,
                theme,
            ))
            .child(c::outline_required_text(
                format!("{id}-text"),
                text.to_owned(),
                true,
                selected,
                theme,
            ))
            .child(c::outline_action_metadata(theme).child("Project · Review"))
            .child(c::outline_required_lifecycle(
                format!("{id}-lifecycle"),
                "Open".into(),
                theme,
            ))
            .child(c::outline_required_fact(
                format!("{id}-fact"),
                "Branch: gallery-screens".into(),
                selected,
                theme,
            ))
            .child(c::outline_request_link(
                format!("{id}-link").into(),
                "Review".into(),
                theme,
            ))
            .child(c::outline_artifact_link(
                format!("{id}-artifact").into(),
                "Plan".into(),
                theme,
            ))
            .child(
                c::outline_approval_card(format!("{id}-approval"), theme)
                    .child("Approve this change"),
            )
            .into_any_element(),
    };
    div()
        .when(disabled, |el| el.opacity(0.45))
        .child(element)
        .into_any_element()
}

pub fn grid(
    story: Story,
    theme: &Theme,
    overrides: &mast_ui::style_overrides::Overrides,
) -> impl IntoElement {
    let layout = theme.ui_style.layout;
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div().flex().child(div().w(px(100.))).children(
                UiStyleId::ALL
                    .iter()
                    .map(|id| div().flex_1().child(id.label())),
            ),
        )
        .children(STATES.iter().enumerate().map(|(row, &(state, label))| {
            div()
                .flex()
                .gap(px(8.))
                .items_center()
                .child(div().w(px(92.)).child(label))
                .children(UiStyleId::ALL.iter().enumerate().map(|(column, &style)| {
                    let view_theme = ui_style::theme_for_view(theme, style, layout, overrides);
                    div()
                        .flex_1()
                        .min_w_0()
                        .p(px(8.))
                        .when(
                            matches!(story, Story::Button | Story::Toolbar | Story::Chip),
                            |el| el.flex(),
                        )
                        .child(fixture(
                            story,
                            state,
                            &view_theme,
                            format!("fixture-{row}-{column}"),
                        ))
                }))
        }))
}

/// A fixture-only status board composed from the outline's actual row and card chrome.
pub fn board(theme: &Theme, overrides: &mast_ui::style_overrides::Overrides) -> impl IntoElement {
    let layout = theme.ui_style.layout;
    div()
        .id("board-comparison")
        .overflow_x_scroll()
        .flex()
        .gap(px(16.))
        .children(
            UiStyleId::ALL
                .iter()
                .enumerate()
                .map(|(style_index, &style)| {
                    let view = ui_style::theme_for_view(theme, style, layout, overrides);
                    let lanes: [(&str, &[&str]); 4] = [
                        ("Needs you", &["Review the design", "Choose a theme"]),
                        ("Running", &["Build the gallery", "Check component states"]),
                        ("Ready", &["Review the watcher"]),
                        ("Done", &["Audit shared controls", "Add fixtures"]),
                    ];
                    div()
                        .w(px(700.))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .child(div().text_color(view.text).child(style.label()))
                        .child(div().flex().gap(px(8.)).children(
                            lanes.into_iter().enumerate().map(|(lane, (name, items))| {
                                let color = match lane {
                                    0 => view.warning,
                                    1 => view.accent,
                                    2 => view.success,
                                    _ => view.text_muted,
                                };
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(8.))
                                    .child(c::outline_lane_heading(
                                        name.to_owned(),
                                        color,
                                        0,
                                        &view,
                                    ))
                                    .children(items.iter().enumerate().map(|(index, title)| {
                                        let id = format!("board-{style_index}-{lane}-{index}");
                                        c::outline_approval_card(id.clone(), &view).p(px(6.)).child(
                                            c::outline_row(id.into(), 38., 4., &view).child(
                                                div().min_w_0().overflow_hidden().child(*title),
                                            ),
                                        )
                                    }))
                            }),
                        ))
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_has_unique_names_and_states() {
        let names: std::collections::HashSet<_> = STORIES.iter().map(|(_, name)| name).collect();
        assert_eq!(names.len(), STORIES.len());
        let names: std::collections::HashSet<_> = STATES.iter().map(|(_, name)| name).collect();
        assert_eq!(names.len(), STATES.len());
    }
}
