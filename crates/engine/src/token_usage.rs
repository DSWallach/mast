//! Local usage ledger. Provider accounting and payload diagnostics stay separate.
use crate::EngineError;
use mast_proto::{AgentEvent, DoneStatus, RunRequest, ToolCall, token_usage::*};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

mod query;
mod retention;
#[cfg(test)]
mod tests;

fn fail(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("Token usage: {e}"))
}
fn fingerprint(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

#[derive(Default)]
struct Active {
    owner: String,
    run: String,
    turn: u64,
    model: String,
    activity: String,
    configuration: String,
    base_configuration: String,
    has_turn: bool,
    context: UsageContext,
    calls: HashMap<String, (String, String, std::time::Instant)>,
}
struct Inner {
    db: Connection,
    active: HashMap<String, Active>,
    signature_key: String,
    capture_failures: u64,
}
#[derive(Clone)]
pub struct TokenUsage(Arc<Mutex<Inner>>, Arc<std::path::PathBuf>);

pub struct CaptureGuard {
    usage: TokenUsage,
    chat: String,
    run: String,
}
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        let mut inner = self.usage.0.lock().unwrap_or_else(|p| p.into_inner());
        if inner
            .active
            .get(&self.chat)
            .is_some_and(|active| active.run == self.run)
        {
            inner.active.remove(&self.chat);
        }
    }
}

impl TokenUsage {
    pub fn guard(&self, chat: &str, run: &str) -> CaptureGuard {
        CaptureGuard {
            usage: self.clone(),
            chat: chat.into(),
            run: run.into(),
        }
    }
    pub fn open(root: &Path) -> Result<Self, EngineError> {
        let path = root.join("token-usage.sqlite3");
        let db = Connection::open(&path).map_err(fail)?;
        db.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(fail)?;
        let version: u32 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(fail)?;
        if version > 1 {
            return Err(fail(
                "ledger belongs to a newer or unreleased development version",
            ));
        }
        db.execute_batch("PRAGMA journal_mode=WAL; BEGIN IMMEDIATE;
            CREATE TABLE IF NOT EXISTS turns(id TEXT PRIMARY KEY,chat TEXT NOT NULL,activity TEXT NOT NULL,configuration TEXT NOT NULL,at INTEGER NOT NULL,status TEXT NOT NULL,context TEXT NOT NULL DEFAULT '{}');
            CREATE TABLE IF NOT EXISTS measurements(turn_id TEXT NOT NULL,source TEXT NOT NULL,model TEXT NOT NULL,role TEXT NOT NULL,scope TEXT NOT NULL,input INTEGER NOT NULL,output INTEGER NOT NULL,cache_read INTEGER,cache_write INTEGER,reasoning INTEGER,PRIMARY KEY(turn_id,source));
            CREATE TABLE IF NOT EXISTS diagnostics(turn_id TEXT NOT NULL,source TEXT NOT NULL,category TEXT NOT NULL,label TEXT NOT NULL,revision TEXT,channel TEXT,bytes INTEGER NOT NULL,stage TEXT NOT NULL,method TEXT NOT NULL,is_error INTEGER,signature TEXT,elapsed_ms INTEGER,PRIMARY KEY(turn_id,source,category,stage));
            CREATE TABLE IF NOT EXISTS context_samples(turn_id TEXT NOT NULL,role TEXT NOT NULL,parent TEXT NOT NULL,at INTEGER NOT NULL,tokens INTEGER,window INTEGER,PRIMARY KEY(turn_id,parent,at));
            CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS turns_time ON turns(at);
            CREATE INDEX IF NOT EXISTS turns_chat ON turns(chat);
            CREATE INDEX IF NOT EXISTS measurements_model ON measurements(model,turn_id);
            CREATE INDEX IF NOT EXISTS diagnostics_turn ON diagnostics(turn_id);
            CREATE INDEX IF NOT EXISTS context_samples_turn ON context_samples(turn_id);
            CREATE UNIQUE INDEX IF NOT EXISTS provider_source ON measurements(source) WHERE substr(source,1,9)='provider:';
            PRAGMA user_version=1; COMMIT;").map_err(fail)?;
        db.execute(
            "INSERT OR IGNORE INTO metadata VALUES('signatureKey',?1)",
            [crate::new_id()],
        )
        .map_err(fail)?;
        let signature_key = db
            .query_row(
                "SELECT value FROM metadata WHERE key='signatureKey'",
                [],
                |r| r.get(0),
            )
            .map_err(fail)?;
        Ok(Self(
            Arc::new(Mutex::new(Inner {
                db,
                active: HashMap::new(),
                signature_key,
                capture_failures: 0,
            })),
            Arc::new(path),
        ))
    }

    pub fn begin(&self, chat: &str, run: &str, request: &RunRequest, activity: &str) {
        self.begin_with_owner(chat, chat, run, request, activity);
    }
    fn begin_with_owner(
        &self,
        chat: &str,
        owner: &str,
        run: &str,
        request: &RunRequest,
        activity: &str,
    ) {
        let configuration = fingerprint(&serde_json::json!({"build":env!("CARGO_PKG_VERSION"), "harness": request.harness, "model":request.model, "reasoning":request.reasoning, "options":request.model_options, "instructions":request.instructions.as_ref().map(|d| &d.envelope.revision_hash)}).to_string());
        let mut inner = self.0.lock().unwrap_or_else(|p| p.into_inner());
        inner.active.insert(
            chat.into(),
            Active {
                owner: owner.into(),
                run: run.into(),
                activity: activity.into(),
                base_configuration: configuration.clone(),
                configuration,
                has_turn: true,
                context: UsageContext {
                    harness: request.harness.map(|h| format!("{h:?}")),
                    reasoning: request.reasoning.map(|r| format!("{r:?}")),
                    configured_model: request.model.clone(),
                    build: Some(env!("CARGO_PKG_VERSION").into()),
                    ..Default::default()
                },
                model: "Unknown model".into(),
                ..Default::default()
            },
        );
        drop(inner);
        self.observe(
            chat,
            &AgentEvent::UsageDiagnostic {
                diagnostic: UsageDiagnostic {
                    id: "prompt".into(),
                    category: "prompt".into(),
                    label: "User prompt".into(),
                    bytes: request.prompt.len() as u64,
                    stage: "submittedToHarness".into(),
                    method: "bytesOnly".into(),
                    ..Default::default()
                },
            },
        );
        // Inventory is explicitly not proof of prompt inclusion. It lets the
        // user find large repo rules even when a harness loads them privately.
        if let Some(directory) = (activity != "Title generation"
            && Path::new(&request.cwd).is_absolute())
        .then_some(Path::new(&request.cwd))
        {
            for name in ["AGENTS.md", "CLAUDE.md"] {
                if let Some(diagnostic) = instruction_file(
                    &directory.join(name),
                    format!("available:{name}:{}", directory.display()),
                    "availableOnDisk",
                ) {
                    self.observe(chat, &AgentEvent::UsageDiagnostic { diagnostic });
                }
            }
        }
        if let Some(delivery) = &request.instructions {
            self.observe(
                chat,
                &AgentEvent::UsageDiagnostic {
                    diagnostic: UsageDiagnostic {
                        id: "instructionEnvelope".into(),
                        category: "instructionEnvelope".into(),
                        label: "Work Outline instruction envelope".into(),
                        revision: Some(delivery.envelope.revision_hash.clone()),
                        bytes: delivery.envelope.rendered.len() as u64,
                        stage: "submittedToHarness".into(),
                        method: "bytesOnly".into(),
                        ..Default::default()
                    },
                },
            );
        }
    }

    pub fn set_context(
        &self,
        chat: &str,
        space: Option<String>,
        item: Option<String>,
        logical_turn: Option<String>,
    ) {
        let mut inner = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let Some(active) = inner.active.get_mut(chat) else {
            return;
        };
        active.context.space_id = space;
        active.context.work_item_id = item;
        active.context.logical_turn = logical_turn;
        let turn = format!("{}:{}", active.run, active.turn);
        let context = serde_json::to_string(&active.context).unwrap_or_default();
        if let Err(error) = inner.db.execute(
            "UPDATE turns SET context=?2 WHERE id=?1",
            params![turn, context],
        ) {
            tracing::warn!(%error,"usage context write failed");
        }
    }

    pub fn begin_title(&self, chat: &str, request: &RunRequest) -> String {
        let key = format!("title:{}", crate::new_id());
        self.begin_with_owner(&key, chat, &key, request, "Title generation");
        key
    }
    pub fn finish_title(&self, key: &str) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .active
            .remove(key);
    }

    pub fn activate(&self, chat: &str) {
        if let Some(active) = self
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .active
            .get_mut(chat)
        {
            active.has_turn = true;
        }
    }
    pub fn observe(&self, chat: &str, event: &AgentEvent) {
        let mut leaf = event;
        let mut parent = String::new();
        while let AgentEvent::Subagent {
            parent_tool_use_id,
            event,
        } = leaf
        {
            parent.push_str(parent_tool_use_id);
            parent.push('/');
            leaf = event;
        }
        // Text deltas never take the shared lock or touch SQLite.
        if !matches!(
            leaf,
            AgentEvent::Usage { .. }
                | AgentEvent::UsageMeasurement { .. }
                | AgentEvent::UsageDiagnostic { .. }
                | AgentEvent::SessionStarted { .. }
                | AgentEvent::ToolCall { .. }
                | AgentEvent::ToolResult { .. }
                | AgentEvent::Done { .. }
                | AgentEvent::ContextUsage { .. }
                | AgentEvent::Steered { .. }
        ) {
            return;
        }
        // File metadata is read before entering the collector critical section.
        let file = if let AgentEvent::ToolCall {
            id,
            call: ToolCall::ReadFile { path },
        } = leaf
        {
            instruction_file(
                Path::new(path),
                format!("instructionRead:{parent}{id}"),
                "requestedRead",
            )
        } else {
            None
        };
        let mut inner = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let result = (|| {
            inner.db.execute_batch("BEGIN IMMEDIATE").map_err(fail)?;
            inner.observe(chat, event)?;
            if let Some(diagnostic) = file {
                inner.observe(chat, &AgentEvent::UsageDiagnostic { diagnostic })?;
            }
            inner.db.execute_batch("COMMIT").map_err(fail)
        })();
        if let Err(error) = result {
            let _ = inner.db.execute_batch("ROLLBACK");
            inner.capture_failures += 1;
            tracing::warn!(%error,"token usage capture failed");
        }
    }

    pub fn delete_chat(&self, chat: &str) -> Result<(), EngineError> {
        let mut inner = self.0.lock().unwrap_or_else(|p| p.into_inner());
        inner.active.retain(|_, active| active.owner != chat);
        let tx = inner.db.transaction().map_err(fail)?;
        for table in ["measurements", "diagnostics", "context_samples"] {
            tx.execute(
                &format!(
                    "DELETE FROM {table} WHERE turn_id IN (SELECT id FROM turns WHERE chat=?1)"
                ),
                [chat],
            )
            .map_err(fail)?;
        }
        tx.execute("DELETE FROM turns WHERE chat=?1", [chat])
            .map_err(fail)?;
        tx.commit().map_err(fail)?;
        inner.active.retain(|_, active| active.owner != chat);
        Ok(())
    }
}

impl Inner {
    fn observe(&mut self, chat: &str, event: &AgentEvent) -> Result<(), EngineError> {
        let Some(active) = self.active.get_mut(chat) else {
            return Ok(());
        };
        let mut leaf = event;
        let mut role = "main";
        let mut parent = String::new();
        while let AgentEvent::Subagent {
            parent_tool_use_id,
            event,
        } = leaf
        {
            role = "subagent";
            parent.push_str(parent_tool_use_id);
            parent.push('/');
            leaf = event;
        }
        if role == "main" && matches!(leaf, AgentEvent::Steered { .. }) {
            active.has_turn = true;
            return Ok(());
        }
        if role == "main" && !active.has_turn && matches!(leaf, AgentEvent::Done { .. }) {
            return Ok(());
        }
        let turn = format!(
            "{}:{}",
            active.run,
            if active.has_turn {
                active.turn
            } else {
                active.turn.saturating_sub(1)
            }
        );
        // Avoid writes for the high-frequency text/reasoning stream.
        if !matches!(
            leaf,
            AgentEvent::Usage { .. }
                | AgentEvent::UsageMeasurement { .. }
                | AgentEvent::UsageDiagnostic { .. }
                | AgentEvent::SessionStarted { .. }
                | AgentEvent::ToolCall { .. }
                | AgentEvent::ToolResult { .. }
                | AgentEvent::Done { .. }
                | AgentEvent::ContextUsage { .. }
        ) {
            return Ok(());
        }
        self.db
            .execute(
                "INSERT OR IGNORE INTO turns VALUES(?1,?2,?3,?4,?5,'partial',?6)",
                params![
                    turn,
                    active.owner,
                    active.activity,
                    active.configuration,
                    crate::now_ms(),
                    serde_json::to_string(&active.context).map_err(fail)?
                ],
            )
            .map_err(fail)?;
        match leaf {
            AgentEvent::SessionStarted { model, tools, .. } if role == "main" => {
                if !model.is_empty() {
                    active.model = model.clone();
                }
                let mut names = tools.clone();
                names.sort();
                let revision = fingerprint(&names.join("\n"));
                active.configuration =
                    fingerprint(&format!("{}:{revision}", active.base_configuration));
                self.db
                    .execute(
                        "UPDATE turns SET configuration=?2 WHERE id=?1",
                        params![turn, active.configuration],
                    )
                    .map_err(fail)?;
                write_diagnostic(
                    &self.db,
                    &turn,
                    &UsageDiagnostic {
                        id: "toolNames".into(),
                        category: "toolCatalogNames".into(),
                        label: format!("Advertised names ({} tools)", tools.len()),
                        revision: Some(revision),
                        bytes: tools.iter().map(|t| t.len() as u64).sum(),
                        stage: "advertisedNamesOnly".into(),
                        method: "bytesOnly".into(),
                        ..Default::default()
                    },
                )?;
            }
            AgentEvent::ContextUsage { tokens, window } => {
                self.db
                    .execute(
                        "INSERT OR REPLACE INTO context_samples VALUES(?1,?2,?3,?4,?5,?6)",
                        params![
                            turn,
                            role,
                            parent,
                            crate::now_ms(),
                            tokens.map(|n| n.min(i64::MAX as u64)),
                            window.map(|n| n.min(i64::MAX as u64))
                        ],
                    )
                    .map_err(fail)?;
            }
            AgentEvent::UsageMeasurement { measurement } => {
                let model = measurement.model.as_deref().unwrap_or(if role == "main" {
                    &active.model
                } else {
                    "Unknown model"
                });
                let role = if measurement.scope == "turnIncludingChildren" {
                    "main + subagents"
                } else {
                    role
                };
                write_measurement(
                    &self.db,
                    &turn,
                    &if measurement.id.starts_with("provider:") {
                        format!("provider:{chat}:{}", measurement.id)
                    } else {
                        format!("{parent}{}", measurement.id)
                    },
                    model,
                    role,
                    &measurement.scope,
                    &measurement.counts,
                )?;
            }
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                write_measurement(
                    &self.db,
                    &turn,
                    &format!("{parent}legacy"),
                    &active.model,
                    role,
                    "legacy",
                    &TokenCounts {
                        input: *input_tokens,
                        output: *output_tokens,
                        ..Default::default()
                    },
                )?;
            }
            AgentEvent::ToolCall { id, call } => {
                let (channel, label) = tool_label(call);
                let key = format!("{parent}{id}");
                // Fingerprint only normalized input. The per-ledger secret
                // prevents correlation of identical inputs across different ledgers.
                // The key is in this ledger and does not protect an exfiltrated DB.
                let signature = match call {
                    ToolCall::Exec { command } => {
                        Some(fingerprint(&format!("{}:{command}", self.signature_key)))
                    }
                    ToolCall::Mcp {
                        input: Some(input), ..
                    } => Some(fingerprint(&format!("{}:{input}", self.signature_key))),
                    _ => None,
                };
                active
                    .calls
                    .entry(key.clone())
                    .or_insert_with(|| (channel.clone(), label.clone(), std::time::Instant::now()));
                self.db.execute("UPDATE diagnostics SET channel=?3,label=?4 WHERE turn_id=?1 AND source=?2 AND category='toolArguments'",params![turn,key,channel,label]).map_err(fail)?;
                // The normalized representation may omit original arguments.
                let diagnostic = UsageDiagnostic {
                    id: key,
                    category: "toolCall".into(),
                    label,
                    channel: Some(channel),
                    signature,
                    bytes: 0,
                    stage: "observedCall".into(),
                    method: "countOnly".into(),
                    ..Default::default()
                };
                write_diagnostic(&self.db, &turn, &diagnostic)?;
            }
            AgentEvent::ToolResult {
                id,
                is_error,
                output,
                ..
            } => {
                let key = format!("{parent}{id}");
                let info = active.calls.get(&key);
                let (channel, label) = info
                    .map(|(c, l, _)| (c.clone(), l.clone()))
                    .unwrap_or(("unknown".into(), "Unknown tool".into()));
                let elapsed_ms = info
                    .map(|(_, _, start)| start.elapsed().as_millis().min(u64::MAX as u128) as u64);
                write_diagnostic(
                    &self.db,
                    &turn,
                    &UsageDiagnostic {
                        id: key,
                        category: "retainedToolResult".into(),
                        label,
                        channel: Some(channel),
                        elapsed_ms,
                        signature: output
                            .as_ref()
                            .map(|s| fingerprint(&format!("{}:{s}", self.signature_key))),
                        bytes: output.as_ref().map_or(0, |s| s.len() as u64),
                        stage: "retainedTranscript".into(),
                        method: if output.is_some() {
                            "bytesOnly"
                        } else {
                            "countOnly"
                        }
                        .into(),
                        is_error: Some(*is_error),
                        ..Default::default()
                    },
                )?;
            }
            AgentEvent::UsageDiagnostic { diagnostic } => {
                let mut d = diagnostic.clone();
                d.id = format!("{parent}{}", d.id);
                if matches!(d.category.as_str(), "toolArguments" | "toolResult")
                    && let Some((channel, label, _)) = active.calls.get(&d.id)
                {
                    d.channel = Some(channel.clone());
                    d.label = label.clone();
                }
                write_diagnostic(&self.db, &turn, &d)?;
            }
            AgentEvent::Done { status, .. } if role == "main" => {
                let status = match status {
                    DoneStatus::Completed => "completed",
                    DoneStatus::Errored => "failed",
                    DoneStatus::Interrupted => "cancelled",
                };
                self.db
                    .execute(
                        "UPDATE turns SET status=?2 WHERE id=?1",
                        params![turn, status],
                    )
                    .map_err(fail)?;
                active.has_turn = false;
                active.turn += 1;
                active.calls.clear();
            }
            _ => {}
        }
        Ok(())
    }
}

fn write_measurement(
    db: &Connection,
    turn: &str,
    source: &str,
    model: &str,
    role: &str,
    scope: &str,
    c: &TokenCounts,
) -> Result<(), EngineError> {
    db.execute("INSERT INTO measurements VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT DO UPDATE SET model=CASE WHEN excluded.model='Unknown model' THEN model ELSE excluded.model END,input=MAX(input,excluded.input),output=MAX(output,excluded.output),cache_read=COALESCE(excluded.cache_read,cache_read),cache_write=COALESCE(excluded.cache_write,cache_write),reasoning=COALESCE(excluded.reasoning,reasoning)",params![turn,source,model,role,scope,c.input.min(i64::MAX as u64),c.output.min(i64::MAX as u64),c.cache_read.map(|n|n.min(i64::MAX as u64)),c.cache_write.map(|n|n.min(i64::MAX as u64)),c.reasoning.map(|n|n.min(i64::MAX as u64))]).map_err(fail)?;
    Ok(())
}
fn write_diagnostic(db: &Connection, turn: &str, d: &UsageDiagnostic) -> Result<(), EngineError> {
    db.execute("INSERT INTO diagnostics VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) ON CONFLICT(turn_id,source,category,stage) DO UPDATE SET bytes=MAX(bytes,excluded.bytes),label=excluded.label,channel=excluded.channel,is_error=excluded.is_error",params![turn,d.id,d.category,d.label,d.revision,d.channel,d.bytes.min(i64::MAX as u64),d.stage,d.method,d.is_error,d.signature,d.elapsed_ms]).map_err(fail)?;
    Ok(())
}
fn tool_label(call: &ToolCall) -> (String, String) {
    match call {
        ToolCall::Mcp { server, tool, .. } => ("mcp".into(), format!("{server}/{tool}")),
        ToolCall::Exec { command } => {
            // Only recognize simple argv. Never retain arbitrary shell text.
            let words = if command.contains(['|', ';', '&', '\n', '`', '$', '>', '<']) {
                None
            } else {
                shell_words::split(command).ok()
            };
            let exe = words
                .and_then(|w| w.first().cloned())
                .and_then(|s| Path::new(&s).file_name()?.to_str().map(str::to_owned))
                .filter(|s| {
                    s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
                });
            (
                "cli".into(),
                exe.unwrap_or("Shell script / unknown executable".into()),
            )
        }
        ToolCall::ReadFile { .. } => ("native".into(), "Read file".into()),
        ToolCall::Search { .. } | ToolCall::Glob { .. } => ("native".into(), "Search files".into()),
        _ => ("native".into(), "Other native tool".into()),
    }
}

fn instruction_file(path: &Path, id: String, stage: &str) -> Option<UsageDiagnostic> {
    if !path.is_absolute() {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    if !matches!(name, "AGENTS.md" | "CLAUDE.md" | "SKILL.md") {
        return None;
    }
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(UsageDiagnostic {
        id,
        category: "instructionFile".into(),
        label: if name == "SKILL.md" {
            format!("{}/SKILL.md", path.parent()?.file_name()?.to_str()?)
        } else {
            name.to_owned()
        },
        revision: Some(fingerprint(&format!(
            "file-metadata:{modified}:{}",
            metadata.len()
        ))),
        bytes: metadata.len(),
        stage: stage.into(),
        method: "bytesOnly".into(),
        ..Default::default()
    })
}

pub(crate) fn classify_activity(
    request: &RunRequest,
    links: &[mast_proto::ChatPrLink],
    worker: bool,
) -> &'static str {
    if request.scheduled_task.is_some() {
        "Scheduled task"
    } else if links
        .iter()
        .any(|link| link.source == mast_proto::ChatLinkSource::ReviewLauncher)
    {
        "PR review"
    } else if worker
        || !matches!(
            mast_proto::message_source::classify(&request.prompt).0,
            mast_proto::message_source::MessageSource::Human
        )
    {
        "Work-item automation"
    } else {
        "Conversation"
    }
}
