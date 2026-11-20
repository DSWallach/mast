use super::*;

// ---------------------------------------------------------------------------
// Highlight store (background, time-sliced, paint-only)
// ---------------------------------------------------------------------------

pub(super) struct HighlightEntry {
    key: DocumentHighlightKey,
    document: Option<Weak<mast_syntax::HighlightedDocument>>,
    _task: Option<Task<()>>,
}

/// Cache of tokenized code blocks keyed by `(row id, block ix)`. Tokenization
/// runs on the background executor, time-sliced; results apply as paint-only
/// run colors when they land.
#[derive(Default)]
pub(super) struct HighlightStore {
    pub(super) entries: HashMap<(SharedString, usize), HighlightEntry>,
    cache: SyntaxHighlightCache,
}

impl HighlightStore {
    /// Current tokens if ready; kicks a background tokenize when stale/missing.
    pub(super) fn request(
        &mut self,
        row_id: SharedString,
        block_ix: usize,
        lang: Lang,
        code: &str,
        cx: &mut Context<Transcript>,
    ) -> Option<Arc<mast_syntax::HighlightedDocument>> {
        let slot_key = (row_id.clone(), block_ix);
        let document_key = DocumentHighlightKey::new(lang, code);
        if let Some(entry) = self.entries.get(&slot_key)
            && entry.key == document_key
        {
            let document = entry.document.as_ref()?;
            if let Some(document) = document.upgrade() {
                return Some(document);
            }
        }
        if let Some(document) = self.cache.get(&document_key) {
            self.entries.insert(
                slot_key,
                HighlightEntry {
                    key: document_key,
                    document: Some(Arc::downgrade(&document)),
                    _task: None,
                },
            );
            return Some(document);
        }
        let code = code.to_string();
        let source_bytes = code.len();
        let task = cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let document = cx
                .background_executor()
                .spawn(async move {
                    mast_syntax::highlight(mast_syntax::HighlightRequest {
                        source: &code,
                        path: None,
                        fence_tag: Some(match lang {
                            Lang::Rust => "rust",
                            Lang::JavaScript => "javascript",
                            Lang::Jsx => "jsx",
                            Lang::TypeScript => "typescript",
                            Lang::Tsx => "tsx",
                            Lang::Python => "python",
                            Lang::Go => "go",
                            Lang::Json => "json",
                            Lang::Jsonc => "jsonc",
                            Lang::Bash => "bash",
                            Lang::Toml => "toml",
                            Lang::Markdown => "markdown",
                            Lang::Html => "html",
                            Lang::Css => "css",
                            Lang::Yaml => "yaml",
                            Lang::C => "c",
                            Lang::Cpp => "cpp",
                            Lang::CSharp => "csharp",
                            Lang::Java => "java",
                            Lang::Kotlin => "kotlin",
                            Lang::Swift => "swift",
                            Lang::Ruby => "ruby",
                            Lang::Php => "php",
                            Lang::Sql => "sql",
                            Lang::Lua => "lua",
                            Lang::Dockerfile => "dockerfile",
                            Lang::Nix => "nix",
                            Lang::Make => "make",
                        }),
                    })
                    .ok()
                })
                .await;
            this.update(cx, |transcript, cx| {
                if let Some(document) = document {
                    let document = Arc::new(document);
                    let retained = transcript
                        .highlights
                        .cache
                        .insert(document_key, document.clone());
                    if let Some(entry) = transcript.highlights.entries.get_mut(&slot_key)
                        && entry.key == document_key
                    {
                        tracing::debug!(
                            language = ?lang,
                            source_bytes,
                            spans = document.lines.iter().map(Vec::len).sum::<usize>(),
                            elapsed_us = started.elapsed().as_micros() as u64,
                            "syntax highlight ready"
                        );
                        entry.document = retained.then(|| Arc::downgrade(&document));
                        cx.notify();
                    }
                }
            })
            .ok();
        });
        self.entries.insert(
            (row_id, block_ix),
            HighlightEntry {
                key: document_key,
                document: None,
                _task: Some(task),
            },
        );
        None
    }
}

// ---------------------------------------------------------------------------
// Transcript entity
// ---------------------------------------------------------------------------

/// Expensive presentation work belongs to the subscription's background job,
/// never to a GPUI observer/render callback. Shared rows survive navigation.
#[derive(Default)]
pub(crate) struct TranscriptPreparation {
    entries: Vec<SessionMessageEntry>,
    cache: HashMap<String, Arc<Vec<Row>>>,
    live_parsers: HashMap<String, IncrementalParser>,
    tree_cache: HashMap<String, (usize, Arc<BlockTree>)>,
    baseline: Option<mast_doc::TranscriptBaseline>,
}

pub(crate) struct PreparedTranscript {
    pub(crate) rows: HashMap<String, Arc<Vec<Row>>>,
    pub(crate) historical: HashMap<String, Vec<Row>>,
    pub(super) fully_historical: HashSet<String>,
    pub(crate) navigation_baseline: Arc<mast_doc::TranscriptBaseline>,
    pub(crate) bytes: usize,
}

impl TranscriptPreparation {
    pub(crate) fn prepare(
        &mut self,
        update: &mast_doc::TranscriptUpdate,
    ) -> Result<Arc<PreparedTranscript>, mast_doc::TranscriptDesync> {
        match &update.frame {
            mast_doc::TranscriptFrame::Reset { .. } => {
                self.cache.clear();
                self.tree_cache.clear();
                self.live_parsers.clear();
            }
            mast_doc::TranscriptFrame::Delta {
                upsert,
                append,
                remove,
                ..
            } => {
                if !upsert.is_empty() {
                    self.tree_cache.clear();
                }
                for id in upsert
                    .iter()
                    .map(|u| &u.entry.id)
                    .chain(append.iter().map(|a| &a.entry))
                    .chain(remove.iter())
                {
                    self.cache.remove(id);
                }
            }
        }
        mast_doc::apply_transcript_frame(&mut self.entries, update.frame.clone())?;
        if let Some(baseline) = &update.replay_baseline {
            self.baseline = Some(baseline.clone());
        }
        let mut rows = HashMap::new();
        let mut historical = HashMap::new();
        let mut fully_historical = HashSet::new();
        let mut bytes = 0;
        for entry in &self.entries {
            let built = if let Some(rows) = self.cache.get(&entry.id) {
                rows.clone()
            } else {
                let streaming = entry.status == Some(MessageStatus::Streaming);
                let built = Arc::new(rows_for_entry(entry, false, &mut |key, text| {
                    parse_for_row(
                        streaming,
                        key,
                        text,
                        &mut self.live_parsers,
                        &mut self.tree_cache,
                    )
                    .0
                }));
                self.cache.insert(entry.id.clone(), built.clone());
                built
            };
            rows.insert(entry.id.clone(), built);
            if self.baseline.as_ref().is_some_and(|b| b.covers(entry)) {
                fully_historical.insert(entry.id.clone());
            }
            if let Some(baseline) = &self.baseline
                && !fully_historical.contains(&entry.id)
                && let Some(prefix) = baseline.historical_entry(entry)
            {
                historical.insert(
                    entry.id.clone(),
                    rows_for_entry(&prefix, false, &mut |_, text| Arc::new(parse_full(text))),
                );
            }
            bytes += std::mem::size_of::<SessionMessageEntry>()
                + entry.id.len()
                + entry
                    .parts
                    .iter()
                    .map(|part| std::mem::size_of::<MessagePart>() + part.byte_len())
                    .sum::<usize>();
        }
        self.cache.retain(|id, _| rows.contains_key(id));
        Ok(Arc::new(PreparedTranscript {
            rows,
            historical,
            fully_historical,
            bytes,
            navigation_baseline: Arc::new(mast_doc::TranscriptBaseline::capture(&self.entries)),
        }))
    }
}
