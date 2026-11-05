//! Read immutable copies only. No engine store constructors are used here.
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use loro::LoroDoc;
use mast_doc::{RegistryDoc, SessionDoc, SessionMessageEntry, join_continuation_entries};
use mast_proto::{AgentEvent, Chat};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;

pub struct Snapshot {
    pub registry: RegistryDoc,
    pub chats: Vec<Chat>,
    pub transcripts: HashMap<String, Vec<SessionMessageEntry>>,
    pub journal_events: usize,
    pub malformed_journal_lines: usize,
    pub failed_transcripts: usize,
}

#[derive(Deserialize)]
struct JournalLine {
    event: AgentEvent,
}

fn within(root: &Path, path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize()?;
    if !path.starts_with(root) {
        bail!("snapshot entry escapes snapshot directory");
    }
    Ok(path)
}

pub fn read_snapshot(path: &Path) -> Result<Snapshot> {
    let root = path.canonicalize().context("snapshot directory")?;
    if !root.is_dir() {
        bail!("snapshot path must be a directory");
    }
    if root.components().any(|c| c.as_os_str() == ".zeron") {
        bail!("a live Mast data directory cannot be used as a snapshot");
    }
    let db = within(&root, &root.join("docs.sqlite3"))?;
    if db.components().any(|c| c.as_os_str() == ".zeron") {
        bail!("a live Mast database cannot be used as a snapshot");
    }
    // URI immutable avoids WAL/shared-memory creation even on the copy.
    let uri = format!(
        "file:{}?immutable=1",
        db.to_string_lossy()
            .replace('%', "%25")
            .replace('?', "%3F")
            .replace('#', "%23")
    );
    let connection = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let bytes: Vec<u8> = connection.query_row(
        "SELECT bytes FROM snapshots WHERE doc_id=?1",
        [mast_doc::REGISTRY_DOC_ID],
        |row| row.get(0),
    )?;
    let registry = RegistryDoc::from_bytes(&bytes, "metrics-reader")?;
    let chats = registry.read_chats()?;
    let mut transcripts = HashMap::new();
    let mut failed_transcripts = 0;
    for chat in &chats {
        let bytes: Option<Vec<u8>> = connection
            .query_row(
                "SELECT bytes FROM snapshots WHERE doc_id=?1",
                [&chat.id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(bytes) = bytes {
            let doc = LoroDoc::new();
            match doc
                .import(&bytes)
                .map_err(anyhow::Error::from)
                .and_then(|_| {
                    Ok(join_continuation_entries(
                        SessionDoc::from_doc(doc).read_entries()?,
                    ))
                }) {
                Ok(entries) => {
                    transcripts.insert(chat.id.clone(), entries);
                }
                Err(_) => failed_transcripts += 1,
            }
        }
    }
    let mut journal_events = 0;
    let mut malformed_journal_lines = 0;
    let journals = root.join("journals");
    if journals.exists() {
        let journals = within(&root, &journals)?;
        for entry in fs::read_dir(journals)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "jsonl")
            {
                continue;
            }
            let path = within(&root, &entry.path())?;
            for line in BufReader::new(fs::File::open(path)?).lines() {
                match line
                    .ok()
                    .and_then(|line| serde_json::from_str::<JournalLine>(&line).ok())
                {
                    Some(record) => {
                        let _ = record.event;
                        journal_events += 1;
                    }
                    None => malformed_journal_lines += 1,
                }
            }
        }
    }
    Ok(Snapshot {
        registry,
        chats,
        transcripts,
        journal_events,
        malformed_journal_lines,
        failed_transcripts,
    })
}
