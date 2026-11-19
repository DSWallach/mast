pub(super) mod data;
mod layout;
pub(super) mod view;

use super::Changes;

impl Changes {
    pub(super) fn scope_map_folder_totals(
        &self,
    ) -> std::sync::Arc<std::collections::HashMap<String, data::Totals>> {
        self.scope_map_data
            .get()
            .map(|data| data.folders.clone())
            .unwrap_or_default()
    }

    pub(super) fn refresh_scope_map_data(&mut self) {
        let Some(parsed) = self.parsed.as_ref() else {
            self.scope_map_data.invalidate();
            return;
        };
        let key = parsed.key.clone();
        let files = parsed.files.clone();
        let layout = parsed.layout.clone();
        let risks = self.scope_map_risks.clone();
        self.scope_map_data
            .get_or_build(&key, || data::Data::new(&files, &layout, &risks));
    }
}
