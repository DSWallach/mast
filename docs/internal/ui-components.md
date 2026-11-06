# UI component inventory (in progress)

This is a preliminary index of style-specific references, not a complete component or call-site inventory. Line numbers refer to integration commit `99e0cd38d`. References include declarations and comparisons, not just branches.

## Target appearance model (not implemented yet)

Style, layout, and theme are independent settings. Style selects element design:
Default, Industrial, or Area. Layout selects density and guidance: Minimal,
Compact, or Guided. Theme supplies semantic colours. Components must resolve
look tokens and variants from style, density and guidance from layout, and
colours from the selected theme; no pairing may be assumed. Screens must not
branch on either style or layout. The planned lint must cover both.

The existing `uiStyle` combines these axes. Migration must map `minimal` to
Default + Minimal, `dense` to Default + Compact, `guided` to Default + Guided,
`industrial` to Industrial + Minimal, and `area` to Area + Minimal, then
persist separate `uiStyle` and `uiLayout` values. An explicit `uiLayout` must
remain authoritative on subsequent loads. Appearance needs separate Style,
Layout and Theme pickers. Gallery and preview need independent style and layout
axes, including the requested gallery keys and `--layouts` preview option.

## Style-specific references

- `crates/ui/src/appearance.rs:159`: `crate::ui_style::SurfaceModel::ThemeDefault => surface,`
- `crates/ui/src/appearance.rs:160`: `crate::ui_style::SurfaceModel::Opaque => SurfacePreference::Opaque,`
- `crates/ui/src/appearance.rs:161`: `crate::ui_style::SurfaceModel::Glass => SurfacePreference::Frosted,`
- `crates/ui/src/artifacts/tab.rs:1062`: `.when(theme.ui_style.id == crate::ui_style::UiStyleId::Area, |el| el.rounded_full().border_1().border_color(theme.border_strong))`
- `crates/ui/src/artifacts/tab.rs:1098`: `.when(theme.ui_style.id == crate::ui_style::UiStyleId::Area, |toolbar| {`
- `crates/ui/src/artifacts/tab.rs:1199`: `if theme.ui_style.id != crate::ui_style::UiStyleId::Area { return None; }`
- `crates/ui/src/changes/explanation.rs:451`: `theme.ui_style.controls == crate::ui_style::ControlVariant::Key,`
- `crates/ui/src/changes/explanation.rs:491`: `== crate::ui_style::StatusIndicator::Dot`
- `crates/ui/src/changes/walkthrough.rs:748`: `theme.ui_style.controls == crate::ui_style::ControlVariant::Key,`
- `crates/ui/src/changes/walkthrough.rs:831`: `let industrial = theme.ui_style.controls == crate::ui_style::ControlVariant::Key;`
- `crates/ui/src/changes/walkthrough.rs:1030`: `== crate::ui_style::StatusIndicator::Dot`
- `crates/ui/src/changes/walkthrough.rs:1077`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.self_start().flex().items_center().gap(px(8.0)).px(px(16.0)).py(px(8.0)))`
- `crates/ui/src/changes/walkthrough.rs:1079`: `.when(!selected_reviewed && theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| { let foreground = if crate::theme::contrast_ratio(theme.text, theme.success) >= crate::theme::contrast_ratio(theme.bg, theme.success) { theme.text } else { theme.bg }; el.bg(theme.success).text_color(foreground) })`
- `crates/ui/src/changes/walkthrough.rs:1110`: `.when(self.files.is_empty() && theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.child(`
- `crates/ui/src/changes/walkthrough.rs:1124`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.child(crate::surface_chrome::grille(&theme))),`
- `crates/ui/src/changes/walkthrough.rs:1181`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.font_family(theme.font_mono.clone()))`
- `crates/ui/src/changes/walkthrough.rs:1182`: `.child(if theme.ui_style.controls == crate::ui_style::ControlVariant::Key { crate::surface_chrome::display(&theme).child(progress).into_any_element() } else { div().child(progress).into_any_element() })`
- `crates/ui/src/changes/walkthrough.rs:1188`: `theme.ui_style.controls == crate::ui_style::ControlVariant::Key,`
- `crates/ui/src/changes/walkthrough.rs:1223`: `theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque && theme.ui_style.id != crate::ui_style::UiStyleId::Area,`
- `crates/ui/src/changes/walkthrough.rs:1255`: `== crate::ui_style::SurfaceModel::Opaque,`
- `crates/ui/src/composer.rs:7989`: `.when(!theme.is_frost() && theme.ui_style.surface == crate::ui_style::SurfaceModel::ThemeDefault, |el| el.shadow_lg())`
- `crates/ui/src/composer.rs:8105`: `crate::ui_style::ConversationVariant::Area => (true, false),`
- `crates/ui/src/composer.rs:8106`: `crate::ui_style::ConversationVariant::Industrial => (false, true),`
- `crates/ui/src/composer.rs:8107`: `crate::ui_style::ConversationVariant::Standard => (false, false),`
- `crates/ui/src/composer.rs:8200`: `let area = matches!(theme.ui_style.conversation, crate::ui_style::ConversationVariant::Area);`
- `crates/ui/src/composer.rs:8693`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| crate::surface_chrome::icon_key(el, &theme, false))`
- `crates/ui/src/composer.rs:8742`: `.when(!theme.is_frost() && theme.ui_style.surface == crate::ui_style::SurfaceModel::ThemeDefault, |el| el.shadow_lg());`
- `crates/ui/src/composer.rs:8803`: `.when(theme.ui_style.conversation == crate::ui_style::ConversationVariant::Industrial, |el| el.child(`
- `crates/ui/src/composer.rs:8847`: `crate::ui_style::ConversationVariant::Area => {`
- `crates/ui/src/composer.rs:8858`: `crate::ui_style::ConversationVariant::Standard | crate::ui_style::ConversationVariant::Industrial => if expanded {`
- `crates/ui/src/context_usage.rs:15`: `let industrial = theme.ui_style.controls == crate::ui_style::ControlVariant::Key;`
- `crates/ui/src/files/mod.rs:50`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |element| element.size(px(theme.ui_style.tokens.control_size)))`
- `crates/ui/src/markdown/render.rs:2458`: `if theme.ui_style.id == crate::ui_style::UiStyleId::Area {`
- `crates/ui/src/markdown/render.rs:2585`: `.when(!copied && theme.ui_style.id == crate::ui_style::UiStyleId::Area, |el| el.child("Copy"))`
- `crates/ui/src/markdown/render.rs:2598`: `if theme.ui_style.id == crate::ui_style::UiStyleId::Area {`
- `crates/ui/src/markdown/render.rs:2654`: `.when(theme.ui_style.id == crate::ui_style::UiStyleId::Area, |el| el.rounded(px(14.0)).bg(theme.bg).border_color(theme.bg).p(px(4.0)))`
- `crates/ui/src/markdown/render.rs:2975`: `if theme.ui_style.id == crate::ui_style::UiStyleId::Area {`
- `crates/ui/src/overview.rs:3271`: `let indicator = if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/overview.rs:5522`: `let indicator = if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/popover.rs:335`: `&& theme.ui_style.surface == crate::ui_style::SurfaceModel::ThemeDefault,`
- `crates/ui/src/popover.rs:849`: `if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {`
- `crates/ui/src/popover.rs:1109`: `.when(theme.ui_style.controls != crate::ui_style::ControlVariant::Minimal, |el| el.bg(theme.input_bg).border_1().border_color(theme.border))`
- `crates/ui/src/popover.rs:1143`: `.when(!theme.is_frost() && theme.ui_style.surface == crate::ui_style::SurfaceModel::ThemeDefault, |el| el.shadow_lg())`
- `crates/ui/src/popover.rs:1178`: `.when(theme.ui_style.controls != crate::ui_style::ControlVariant::Minimal, |el| el.bg(theme.input_bg).border_color(theme.border))`
- `crates/ui/src/popover.rs:1214`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.rounded(px(4.0)).bg(theme.accent).border_1().border_color(theme.border).shadow(vec![gpui::BoxShadow { color: crate::surface_chrome::key_ledge(theme), offset: gpui::point(px(0.0), px(2.0)), blur_radius: px(0.0), spread_radius: px(0.0), inset: false }]))`
- `crates/ui/src/popover.rs:1217`: `.text_color(if theme.ui_style.controls == crate::ui_style::ControlVariant::Key { theme.on_accent } else { theme.on_solid })`
- `crates/ui/src/settings/widgets.rs:416`: `if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {`
- `crates/ui/src/settings/widgets.rs:492`: `theme.ui_style.controls == crate::ui_style::ControlVariant::Key,`
- `crates/ui/src/shell/chat_item_ui.rs:295`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface).border_color(theme.border).px(px(16.0)))`
- `crates/ui/src/shell/chat_item_ui.rs:501`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface_raised))`
- `crates/ui/src/shell/chat_item_ui.rs:504`: `.when(is_model && theme.ui_style.controls == crate::ui_style::ControlVariant::Key && tone == ChipTone::Plain, |chip| {`
- `crates/ui/src/shell/chat_item_ui.rs:520`: `if is_model && theme.ui_style.controls == crate::ui_style::ControlVariant::Key && tone == ChipTone::Plain {`
- `crates/ui/src/shell/chat_item_ui.rs:632`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface_raised))`
- `crates/ui/src/shell/chat_item_ui.rs:673`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::ThemeDefault, |el| el.shadow_md())`
- `crates/ui/src/shell/perf_tests.rs:63`: `crate::ui_style::init(crate::ui_style::UiStyleId::Industrial, cx);`
- `crates/ui/src/shell/running_ui.rs:1043`: `let indicator = if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/shell/spaces.rs:2067`: `div().h(px(if theme.ui_style.id == crate::ui_style::UiStyleId::Area { 0.0 } else { theme.ui_style.tokens.rule_width.unwrap_or(1.0) })).bg(if theme.ui_style.tokens.rule_width.is_some() { theme.border } else { theme.border.opacity(0.6) })`
- `crates/ui/src/shell/spaces.rs:2071`: `let key = theme.ui_style.controls == crate::ui_style::ControlVariant::Key || theme.ui_style.id == crate::ui_style::UiStyleId::Area;`
- `crates/ui/src/shell/team_ui.rs:326`: `let indicator = if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/shell.rs:7118`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface))`
- `crates/ui/src/shell.rs:7119`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque && theme.ui_style.id != crate::ui_style::UiStyleId::Area, |el| el.border_r_1().border_color(theme.border))`
- `crates/ui/src/shell.rs:7120`: `.child(if theme.ui_style.id == crate::ui_style::UiStyleId::Area {`
- `crates/ui/src/shell.rs:7299`: `if theme.ui_style.id == crate::ui_style::UiStyleId::Area && search_query.is_none() {`
- `crates/ui/src/shell.rs:7387`: `let glyph = if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/shell.rs:7508`: `let glyph: AnyElement = if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/shell.rs:7549`: `.when(theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot, |el| el.flex().items_center().gap(px(8.0)).child(crate::status_style::indicator_for_theme(crate::status_style::Status::Idle, &theme)))`
- `crates/ui/src/shell.rs:7550`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.font_family(theme.font_mono.clone()))`
- `crates/ui/src/shell.rs:7602`: `let area = theme.ui_style.id == crate::ui_style::UiStyleId::Area;`
- `crates/ui/src/shell.rs:7605`: `let selected_wash = if area { identity } else if theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque { theme.element_active } else { crate::theme::glass_selected_bg() };`
- `crates/ui/src/shell.rs:7841`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.font_family(theme.font_mono.clone()))`
- `crates/ui/src/shell.rs:8462`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.child(`
- `crates/ui/src/shell.rs:10192`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.child(`
- `crates/ui/src/shell.rs:12754`: `let main = if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {`
- `crates/ui/src/shell.rs:12764`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.bg))`
- `crates/ui/src/shell.rs:12765`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.p(px(8.0)).rounded(px(12.0)).bg(theme.surface))`
- `crates/ui/src/shell.rs:12833`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface))`
- `crates/ui/src/shell.rs:12873`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface))`
- `crates/ui/src/shell.rs:12874`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque && theme.ui_style.id != crate::ui_style::UiStyleId::Area, |el| el.border_b_1().border_color(theme.border))`
- `crates/ui/src/status_style.rs:116`: `if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/status_style.rs:124`: `if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/status_style.rs:140`: `&& theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot`
- `crates/ui/src/surface_chrome.rs:20`: `if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {`
- `crates/ui/src/surface_chrome.rs:32`: `if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {`
- `crates/ui/src/surface_chrome.rs:59`: `if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {`
- `crates/ui/src/surface_chrome.rs:66`: `(active && theme.ui_style.controls == crate::ui_style::ControlVariant::Key)`
- `crates/ui/src/surface_chrome.rs:79`: `.when(theme.ui_style.controls != crate::ui_style::ControlVariant::Minimal, |el| el.bg(theme.input_bg).border_1().border_color(theme.border))`
- `crates/ui/src/transcript.rs:6212`: `let area = matches!(conversation, crate::ui_style::ConversationVariant::Area);`
- `crates/ui/src/transcript.rs:6334`: `if matches!(conversation, crate::ui_style::ConversationVariant::Area) {`
- `crates/ui/src/transcript.rs:6344`: `.when(matches!(conversation, crate::ui_style::ConversationVariant::Industrial), |el| el.bg(theme.surface).py(px(8.0)).border_1().border_color(theme.border))`
- `crates/ui/src/transcript.rs:6345`: `.when(matches!(conversation, crate::ui_style::ConversationVariant::Area), |el| el.bg(identity).max_w_full().w_full())`
- `crates/ui/src/transcript.rs:6351`: `crate::ui_style::ConversationVariant::Industrial => div().min_w_0().max_w(px(self.content_width * 0.8)).flex().flex_col().items_end().gap(px(4.0))`
- `crates/ui/src/transcript.rs:6354`: `crate::ui_style::ConversationVariant::Standard | crate::ui_style::ConversationVariant::Area => bubble.into_any_element(),`
- `crates/ui/src/transcript.rs:6573`: `let inner = if matches!(conversation, crate::ui_style::ConversationVariant::Industrial)`
- `crates/ui/src/work_outline/compact_rows_tests.rs:95`: `crate::ui_style::set_style(crate::ui_style::UiStyleId::Industrial, window, cx);`
- `crates/ui/src/work_outline/details_panel.rs:1325`: `.border_color(if theme.ui_style.controls == crate::ui_style::ControlVariant::Key { theme.border } else { theme.accent })`
- `crates/ui/src/work_outline/details_panel.rs:1326`: `.bg(if theme.ui_style.controls == crate::ui_style::ControlVariant::Key { theme.input_bg } else { theme.surface })`
- `crates/ui/src/work_outline/details_panel.rs:1346`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.border_l_2().border_color(theme.industrial_colors().rule))`
- `crates/ui/src/work_outline/details_panel.rs:1496`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.child(`
- `crates/ui/src/work_outline/details_panel.rs:1548`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.flex().items_center().gap(px(8.0)))`
- `crates/ui/src/work_outline/details_panel.rs:1550`: `.when(active && status == WorkItemStatus::Completed && theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| {`
- `crates/ui/src/work_outline/details_panel.rs:1586`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.child(`
- `crates/ui/src/work_outline/details_panel.rs:1598`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface))`
- `crates/ui/src/work_outline/details_panel.rs:1835`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.font_family(theme.font_mono.clone()))`
- `crates/ui/src/work_outline/details_panel.rs:2034`: `div().when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.flex().items_start()).child(`
- `crates/ui/src/work_outline/outline_view.rs:3565`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| {`
- `crates/ui/src/work_outline/outline_view.rs:4100`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface))`
- `crates/ui/src/work_outline/resizable_panes_tests.rs:76`: `crate::ui_style::set_style(crate::ui_style::UiStyleId::Industrial, window, cx);`

## Candidate screen-owned migration slices

- Shell and sidebar.
- Outline and item details.
- Conversation, composer and transcript.
- Changes and walkthrough.
- Settings and appearance.
- Browser and artifacts.
- Linear and Team.
- Menus, popovers and remaining shared surfaces.

Each slice must audit its own call sites and remove screen-owned style branches after the shared components support the existing treatments.
