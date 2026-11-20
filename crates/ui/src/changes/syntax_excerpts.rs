use super::*;

const MAX_EXCERPT_SOURCE_LINES: usize = 200_000;

pub(super) fn excerpt_side(
    file: &FileDiff,
    side: SourceSide,
    language: Lang,
    path: &str,
) -> Option<Arc<mast_syntax::HighlightedDocument>> {
    let max_line = file
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter_map(|line| match side {
            SourceSide::Old => line.old_no,
            SourceSide::New => line.new_no,
        })
        .max()
        .unwrap_or(0) as usize;
    if max_line > MAX_EXCERPT_SOURCE_LINES {
        return None;
    }
    let mut lines = vec![Vec::new(); max_line];
    for hunk in &file.hunks {
        let visible = hunk
            .lines
            .iter()
            .filter_map(|line| {
                let number = match side {
                    SourceSide::Old => line.old_no,
                    SourceSide::New => line.new_no,
                }?;
                // Line numbers start at 1: a synthetic hunk (0,0 ranges) has no real lines.
                (line.kind != LineKind::Meta && number >= 1).then_some((number, line.text.as_str()))
            })
            .collect::<Vec<_>>();
        if visible.is_empty() {
            continue;
        }
        let source = visible
            .iter()
            .map(|(_, text)| *text)
            .collect::<Vec<_>>()
            .join("\n");
        let document = mast_syntax::highlight(mast_syntax::HighlightRequest {
            source: &source,
            path: Some(path),
            fence_tag: None,
        })
        .ok()?;
        for ((number, _), spans) in visible.into_iter().zip(document.lines) {
            if let Some(slot) = lines.get_mut((number as usize).saturating_sub(1)) {
                *slot = spans;
            }
        }
    }
    Some(Arc::new(mast_syntax::HighlightedDocument {
        language,
        lines,
    }))
}

pub(super) fn excerpt_highlights(file: &FileDiff, language: Lang) -> Option<DiffHighlights> {
    if !mast_syntax::supports_language(language) {
        return None;
    }
    let old = if file.status == FileStatus::Added {
        None
    } else {
        Some(excerpt_side(
            file,
            SourceSide::Old,
            language,
            file.old_path.as_deref().unwrap_or(&file.path),
        )?)
    };
    let new = if file.status == FileStatus::Deleted {
        None
    } else {
        Some(excerpt_side(file, SourceSide::New, language, &file.path)?)
    };
    Some(DiffHighlights { old, new, hunk_symbols: Vec::new() })
}

pub(super) fn sources_match_patch(file: &FileDiff, response: &mast_proto::CheckoutFileDiffText) -> bool {
    let old = response
        .old_text
        .as_deref()
        .map(|source| source.lines().collect::<Vec<_>>());
    let new = response
        .new_text
        .as_deref()
        .map(|source| source.lines().collect::<Vec<_>>());
    file.hunks.iter().flat_map(|hunk| &hunk.lines).all(|line| {
        let actual = match line.kind {
            LineKind::Del => line
                .old_no
                .and_then(|number| old.as_ref()?.get(number as usize - 1).copied()),
            LineKind::Add => line
                .new_no
                .and_then(|number| new.as_ref()?.get(number as usize - 1).copied()),
            LineKind::Context => line
                .new_no
                .and_then(|number| new.as_ref()?.get(number as usize - 1).copied())
                .or_else(|| {
                    line.old_no
                        .and_then(|number| old.as_ref()?.get(number as usize - 1).copied())
                }),
            LineKind::Meta => return true,
        };
        actual == Some(line.text.as_str())
    })
}

pub(super) fn full_highlights(
    file: &FileDiff,
    language: Lang,
    response: &mast_proto::CheckoutFileDiffText,
) -> Option<DiffHighlights> {
    if response.stale
        || response.binary
        || response.truncated
        || !sources_match_patch(file, response)
    {
        return None;
    }
    let parse = |source: &str, path: &str| {
        mast_syntax::highlight(mast_syntax::HighlightRequest {
            source,
            path: Some(path),
            fence_tag: None,
        })
        .ok()
        .map(Arc::new)
    };
    let old = match response.old_text.as_deref() {
        Some(source) => Some(parse(
            source,
            file.old_path.as_deref().unwrap_or(&file.path),
        )?),
        None => None,
    };
    let new = match response.new_text.as_deref() {
        Some(source) => Some(parse(source, &file.path)?),
        None => None,
    };
    if old.is_none() && new.is_none() && mast_syntax::supports_language(language) {
        return None;
    }
    let hunk_symbols = response.new_text.as_deref().map(|source| {
        let lines: Vec<_> = file.hunks.iter().map(|hunk| hunk.lines.iter()
            .find(|line| line.kind == LineKind::Add && line.new_no.is_some())
            .or_else(|| hunk.lines.iter().find(|line| line.new_no.is_some()))
            .and_then(|line| line.new_no).unwrap_or(1)).collect();
        mast_syntax::enclosing_symbols(source, &file.path, &lines)
    }).unwrap_or_default();
    Some(DiffHighlights { old, new, hunk_symbols })
}

pub(super) fn file_diff_text_request(
    diff: &CheckoutDiff,
    origin: &FileOrigin,
    mode: String,
    base_ref: Option<String>,
    chat_id: Option<String>,
    commit_sha: Option<String>,
    head_ref: Option<String>,
) -> mast_proto::GetCheckoutFileDiffTextRequest {
    mast_proto::GetCheckoutFileDiffTextRequest {
        checkout_id: diff.checkout_id.clone(),
        cwd: diff.cwd.clone(),
        path: origin.repository_relative_path.clone(),
        repository_path: origin.repository_path.clone(),
        mode,
        base_ref,
        chat_id,
        commit_sha,
        head_ref,
        diff_checksum: diff.checksum.clone(),
    }
}
