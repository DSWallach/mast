//! Composer chips for staged code references.
use super::*;

impl Composer {
    pub(super) fn render_code_refs(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let references = self.state.read(cx).code_refs(&self.current_key).to_vec();
        if references.is_empty() { return None; }
        let mut row = div().w_full().flex_none().flex().flex_row().items_center()
            .gap(px(6.0)).overflow_hidden().px(px(STRIP_PAD_X)).pt(px(STRIP_PAD_TOP));
        for (index, reference) in references.into_iter().enumerate() {
            let id = reference.id.clone();
            let label: SharedString = reference.label().into();
            row = row.child(crate::components::conversation_skill_chip(theme, index)
                .child(label)
                .child(crate::components::conversation_skill_remove(theme, index)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        let key = this.current_key.clone();
                        this.state.update(cx, |state, cx| { state.remove_code_ref(&key, &id); cx.notify(); });
                    }))));
        }
        Some(row)
    }

}
