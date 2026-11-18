use super::*;

impl Changes {
    pub(super) fn request_word_spans(&mut self, key: word_diff::Key, hunk: &Hunk, cx: &mut Context<Self>) -> Option<word_diff::HunkSpans> {
        if !self.word_diff { return None; }
        if let Some(spans) = self.word_spans.get(&key) { return Some(spans); }
        if !self.word_spans.begin(&key) { return None; }
        let lines = if hunk.lines.len() <= word_diff::MAX_HUNK_LINES
            && hunk.lines.iter().map(|line| line.text.len()).sum::<usize>() <= 32_000
        { hunk.lines.clone() } else { Vec::new() };
        cx.spawn(async move |this, cx| {
            let spans = cx.background_executor().spawn(async move { Arc::new(word_diff::spans(&lines)) }).await;
            this.update(cx, |this, cx| { this.word_spans.finish(key, spans); cx.notify(); }).ok();
        }).detach();
        None
    }

    /// Start excerpt parsing and a lazy full-source fetch for an expanded file.
    pub(super) fn request_highlight(
        &mut self,
        file: &FileDiff,
        origin: &FileOrigin,
        parsed_key: &str,
        cx: &mut Context<Self>,
    ) -> Option<Arc<DiffHighlights>> {
        let lang = mast_syntax::language_for_path(&file.path)?;
        let fingerprint = hash64(&[parsed_key, &file.path]);
        if let Some(slot) = self.highlights.get(&file.path)
            && slot.fingerprint == fingerprint
        {
            return match &slot.state {
                DiffHighlightState::Ready(highlights) | DiffHighlightState::Excerpt(highlights) => {
                    Some(highlights.clone())
                }
                DiffHighlightState::Pending | DiffHighlightState::Plain => None,
            };
        }
        if !mast_syntax::supports_language(lang) {
            self.highlights.insert(
                file.path.clone(),
                HighlightSlot {
                    fingerprint,
                    state: DiffHighlightState::Plain,
                    _excerpt_task: None,
                    _fetch_task: None,
                },
            );
            return None;
        }
        #[cfg(test)]
        { self.benchmark_highlight_count += 1; }
        let path = file.path.clone();
        let fetch_origin = origin.clone();
        let excerpt_file = file.clone();
        let excerpt_path = path.clone();
        let excerpt_task = cx.spawn(async move |this, cx| {
            let highlights = cx
                .background_executor()
                .spawn(async move { excerpt_highlights(&excerpt_file, lang).map(Arc::new) })
                .await;
            this.update(cx, |changes, cx| {
                if let Some(slot) = changes.highlights.get_mut(&excerpt_path)
                    && slot.fingerprint == fingerprint
                    && matches!(slot.state, DiffHighlightState::Pending)
                {
                    slot.state = match highlights {
                        Some(highlights) => DiffHighlightState::Excerpt(highlights),
                        None => DiffHighlightState::Plain,
                    };
                    cx.notify();
                }
            })
            .ok();
        });

        let active = self.active_diff(cx).cloned();
        let engine = self.state.read(cx).engine().cloned();
        let target = self.desired_target(cx);
        let chat_id = self.owner_chat_id_owned();
        let mode = self.scope.mode().to_string();
        let (base_ref, head_ref) = match (
            self.scope,
            self.owner.as_ref().and_then(|o| o.range.as_ref()),
        ) {
            (DiffScope::Range, Some(range)) => {
                (Some(range.base_ref.clone()), Some(range.head_ref.clone()))
            }
            _ => (self.base_ref.clone(), None),
        };
        let commit_sha = (self.scope == DiffScope::Commit)
            .then(|| self.commit.as_ref().map(|commit| commit.sha.clone()))
            .flatten();
        let fetch_file = file.clone();
        let fetch_path = path.clone();
        let fetch_task = match (active, engine) {
            (Some(diff), Some(engine)) => Some(cx.spawn(async move |this, cx| {
                let request = file_diff_text_request(
                    &diff,
                    &fetch_origin,
                    mode,
                    base_ref,
                    chat_id,
                    commit_sha,
                    head_ref,
                );
                let mut params = serde_json::to_value(request)
                    .ok()
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default();
                if let Some(target) = target {
                    params.insert("targetDeviceId".into(), serde_json::Value::String(target));
                }
                let response = engine
                    .client()
                    .call(
                        methods::GET_CHECKOUT_FILE_DIFF_TEXT,
                        serde_json::Value::Object(params),
                    )
                    .await
                    .ok()
                    .and_then(|value| {
                        serde_json::from_value::<mast_proto::CheckoutFileDiffText>(value).ok()
                    });
                let highlights = match response {
                    Some(response) => {
                        cx.background_executor()
                            .spawn(async move {
                                full_highlights(&fetch_file, lang, &response).map(Arc::new)
                            })
                            .await
                    }
                    None => None,
                };
                this.update(cx, |changes, cx| {
                    if let Some(slot) = changes.highlights.get_mut(&fetch_path)
                        && slot.fingerprint == fingerprint
                        && let Some(highlights) = highlights
                    {
                        slot.state = DiffHighlightState::Ready(highlights);
                        cx.notify();
                    }
                })
                .ok();
            })),
            _ => None,
        };
        self.highlights.insert(
            file.path.clone(),
            HighlightSlot {
                fingerprint,
                state: DiffHighlightState::Pending,
                _excerpt_task: Some(excerpt_task),
                _fetch_task: fetch_task,
            },
        );
        None
    }


}
