//! Gallery-only alternatives for the Horizon outline hierarchy.
use gpui::{AnyElement, FontWeight, div, prelude::*, px};
use mast_ui::{
    components as c,
    theme::Theme,
    ui_style::{self, UiStyleId},
};

struct Item {
    title: &'static str,
    status: &'static str,
    needs: u8,
    chats: u8,
    selected: bool,
    collapsed: bool,
    children: &'static [Item],
}

const ITEMS: &[Item] = &[
    Item {
        title: "Mast design and product direction",
        status: "Active",
        needs: 1,
        chats: 2,
        selected: false,
        collapsed: false,
        children: &[
            Item {
                title: "Horizon interface and interaction design",
                status: "Active",
                needs: 2,
                chats: 1,
                selected: false,
                collapsed: false,
                children: &[
                    Item {
                        title: "Clarify hierarchy in the work outline",
                        status: "In progress",
                        needs: 1,
                        chats: 2,
                        selected: true,
                        collapsed: false,
                        children: &[
                            Item {
                                title: "Compare light and dark treatments with realistic long titles",
                                status: "Ready",
                                needs: 0,
                                chats: 1,
                                selected: false,
                                collapsed: false,
                                children: &[],
                            },
                            Item {
                                title: "Validate keyboard navigation and selection",
                                status: "Planned",
                                needs: 0,
                                chats: 0,
                                selected: false,
                                collapsed: false,
                                children: &[],
                            },
                        ],
                    },
                    Item {
                        title: "Review composer and transcript refinements",
                        status: "In progress",
                        needs: 0,
                        chats: 2,
                        selected: false,
                        collapsed: true,
                        children: &[Item {
                            title: "Review message spacing",
                            status: "Ready",
                            needs: 0,
                            chats: 1,
                            selected: false,
                            collapsed: false,
                            children: &[],
                        }],
                    },
                ],
            },
            Item {
                title: "Gallery and preview tooling",
                status: "Done",
                needs: 0,
                chats: 1,
                selected: false,
                collapsed: false,
                children: &[],
            },
        ],
    },
    Item {
        title: "Release preparation",
        status: "Planned",
        needs: 0,
        chats: 0,
        selected: false,
        collapsed: true,
        children: &[],
    },
];

pub const LABELS: [&str; 3] = [
    "A · Rounded nesting",
    "B · Indent and type",
    "C · Connected nodes",
];

pub fn tree(
    theme: &Theme,
    overrides: &mast_ui::style_overrides::Overrides,
    option: usize,
) -> impl IntoElement {
    let theme = ui_style::theme_for_view(theme, UiStyleId::Horizon, theme.ui_style.layout, overrides);
    div()
        .w_full()
        .max_w(px(860.))
        .p(px(24.))
        .rounded(px(18.))
        .bg(theme.bg)
        .text_color(theme.text)
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .text_size(px(20.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("Work outline"),
        )
        .child(
            div()
                .pb(px(14.))
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .child("Projects, decisions and conversations"),
        )
        .children(
            ITEMS
                .iter()
                .enumerate()
                .map(|(index, item)| node(item, 0, option, &theme, format!("{index}"))),
        )
}

fn node(item: &Item, depth: usize, option: usize, theme: &Theme, key: String) -> AnyElement {
    let has_children = !item.children.is_empty() || item.collapsed;
    let row_theme = c::outline_row_theme(theme, item.selected);
    let status_color = if item.selected {
        row_theme.text
    } else if item.status == "Done" {
        theme.success
    } else {
        theme.text_muted
    };
    let row = c::outline_row(format!("horizon-tree-{key}").into(), 42., 8., theme)
        .gap(px(8.))
        .child(
            div()
                .w(px(18.))
                .flex_none()
                .text_color(row_theme.text_muted)
                .child(if item.collapsed {
                    "›"
                } else if has_children {
                    "⌄"
                } else {
                    " "
                }),
        )
        .child(div().size(px(7.)).rounded_full().bg(status_color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(row_theme.text)
                .text_size(px(if option == 1 {
                    [16., 14., 13., 12.][depth.min(3)]
                } else {
                    13.
                }))
                .font_weight(if depth == 0 || item.selected {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .child(item.title),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(11.))
                .text_color(row_theme.text_muted)
                .child(item.status),
        )
        .when(item.chats > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(row_theme.text_muted)
                    .child(format!("◌ {}", item.chats)),
            )
        })
        .when(item.needs > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .px(px(7.))
                    .py(px(2.))
                    .rounded_full()
                    .bg(theme.warning.opacity(0.16))
                    .text_color(theme.text)
                    .text_size(px(11.))
                    .child(format!("{} needs you", item.needs)),
            )
        });
    let row = c::outline_row_selection(row, theme, item.selected).max_w(px(800.));
    let children =
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .when(option == 0, |el| {
                el.ml(px(20.))
                    .p(px(5.))
                    .rounded(px(16.))
                    .when(depth < 2, |el| el.bg(theme.text.opacity(0.025)))
            })
            .when(option == 1, |el| el.ml(px(30.)))
            .when(option == 2, |el| {
                el.ml(px(16.))
                    .pl(px(18.))
                    .border_l_1()
                    .border_color(theme.text.opacity(0.12))
            })
            .children(item.children.iter().enumerate().map(|(index, child)| {
                node(child, depth + 1, option, theme, format!("{key}-{index}"))
            }));
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .when(option == 2 && !item.selected, |el| {
            el.rounded(px(14.)).bg(theme.text.opacity(0.025))
        })
        .child(row)
        .when(!item.children.is_empty() && !item.collapsed, |el| {
            el.child(children)
        })
        .into_any_element()
}
