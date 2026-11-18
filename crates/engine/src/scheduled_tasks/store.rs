use super::{evaluator, fail};
use crate::EngineError;
use mast_proto::*;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct Store(Arc<Mutex<Connection>>, Arc<String>);

#[derive(Clone, Copy)]
pub(super) enum Followup {
    Action,
    Archive,
}

impl Followup {
    fn kind(self) -> &'static str {
        match self {
            Self::Action => "action",
            Self::Archive => "archive",
        }
    }

    fn id(self, run: &ScheduledRun) -> &str {
        match self {
            Self::Action => &run.task_id,
            Self::Archive => &run.id,
        }
    }
}

impl Store {
    pub fn open(root: &Path) -> Result<Self, EngineError> {
        blocking(|| {
            let mut c = Connection::open(root.join("scheduled-tasks.sqlite3")).map_err(fail)?;
            let version: i64 = c
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .map_err(fail)?;
            if version > 2 {
                return Err(fail(
                    "This scheduler database was written by a newer Mast version",
                ));
            }
            c.pragma_update(None, "journal_mode", "WAL").map_err(fail)?;
            c.pragma_update(None, "synchronous", "FULL").map_err(fail)?;
            let tx = c.transaction().map_err(fail)?;
            tx.execute_batch("CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY, body TEXT NOT NULL);
              CREATE TABLE IF NOT EXISTS runs(id TEXT PRIMARY KEY, task_id TEXT NOT NULL, slot TEXT NOT NULL, created_at INTEGER NOT NULL, body TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'launching', UNIQUE(task_id,slot));
              CREATE TABLE IF NOT EXISTS operations(id TEXT PRIMARY KEY, input TEXT NOT NULL);
              CREATE TABLE IF NOT EXISTS run_followups(kind TEXT NOT NULL, id TEXT NOT NULL, body TEXT NOT NULL, due INTEGER NOT NULL, PRIMARY KEY(kind,id));
              CREATE INDEX IF NOT EXISTS run_followups_due ON run_followups(kind,due,id);
              CREATE TABLE IF NOT EXISTS notion_writes(kind TEXT NOT NULL, id TEXT NOT NULL, allowed INTEGER NOT NULL, PRIMARY KEY(kind,id));").map_err(fail)?;
            if version == 1 {
                tx.execute_batch("ALTER TABLE runs ADD COLUMN state TEXT NOT NULL DEFAULT 'launching'; UPDATE runs SET state=json_extract(body,'$.state');").map_err(fail)?;
            }
            tx.execute_batch(
                "CREATE INDEX IF NOT EXISTS run_history ON runs(task_id,created_at DESC);
              CREATE INDEX IF NOT EXISTS run_recent ON runs(created_at DESC,id DESC);
              CREATE INDEX IF NOT EXISTS run_active ON runs(state,created_at,id);
              PRAGMA user_version=2;",
            )
            .map_err(fail)?;
            tx.commit().map_err(fail)?;
            Ok(Self(
                Arc::new(Mutex::new(c)),
                Arc::new(root.to_string_lossy().into_owned()),
            ))
        })
    }
    pub fn scope(&self) -> &str {
        self.1.as_str()
    }
    pub fn replay(&self, op: &str, input: &str) -> Result<bool, EngineError> {
        blocking(|| replay(&*self.0.lock().map_err(fail)?, op, input))
    }
    pub fn tasks(&self) -> Result<Vec<ScheduledTask>, EngineError> {
        blocking(|| {
            let c = self.0.lock().map_err(fail)?;
            let mut q = c
                .prepare("SELECT body FROM tasks ORDER BY id")
                .map_err(fail)?;
            let tasks: Vec<ScheduledTask> = q
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(fail)?
                .map(|r| serde_json::from_str(&r.map_err(fail)?).map_err(fail))
                .collect::<Result<_, _>>()?;
            tasks
                .into_iter()
                .map(|mut task| {
                    restore_notion_writes(&c, "task", &task.id, &mut task.draft)?;
                    Ok(task)
                })
                .collect()
        })
    }
    pub fn runs(
        &self,
        id: Option<&str>,
        before: Option<i64>,
    ) -> Result<Vec<ScheduledRun>, EngineError> {
        blocking(|| {
            let c = self.0.lock().map_err(fail)?;
            let mut q=c.prepare("SELECT body FROM runs WHERE (?1 IS NULL OR task_id=?1) AND (?2 IS NULL OR created_at<?2) ORDER BY created_at DESC,id DESC LIMIT 100").map_err(fail)?;
            run_rows(&c, &mut q, params![id, before])
        })
    }
    /// Each task's newest run, which the paginated history can leave out
    /// for a task that runs rarely.
    pub fn latest_per_task(&self) -> Result<Vec<ScheduledRun>, EngineError> {
        blocking(|| {
            let c = self.0.lock().map_err(fail)?;
            let mut q=c.prepare("SELECT body FROM runs r WHERE NOT EXISTS (SELECT 1 FROM runs n WHERE n.task_id=r.task_id AND (n.created_at>r.created_at OR (n.created_at=r.created_at AND n.id>r.id)))").map_err(fail)?;
            run_rows(&c, &mut q, rusqlite::params![])
        })
    }
    pub fn active(&self) -> Result<Vec<ScheduledRun>, EngineError> {
        blocking(|| {
            // Active runs must not disappear behind the paginated history limit.
            let c = self.0.lock().map_err(fail)?;
            let mut q=c.prepare("SELECT body FROM runs WHERE state IN ('launching','queued','running','awaitingInput') ORDER BY created_at,id").map_err(fail)?;
            run_rows(&c, &mut q, rusqlite::params![])
        })
    }
    pub fn save(
        &self,
        task: ScheduledTask,
        expected: u64,
        op: &str,
        input: &str,
    ) -> Result<(), EngineError> {
        blocking(|| {
            let mut c = self.0.lock().map_err(fail)?;
            let tx = c.transaction().map_err(fail)?;
            if replay(&tx, op, input)? {
                return Ok(());
            }
            let previous: Option<String> = tx
                .query_row("SELECT body FROM tasks WHERE id=?1", [&task.id], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(fail)?;
            let rev = previous
                .map(|s| serde_json::from_str::<ScheduledTask>(&s).map(|t| t.revision))
                .transpose()
                .map_err(fail)?
                .unwrap_or(0);
            if rev != expected {
                return Err(fail("Task changed; refresh before saving"));
            }
            tx.execute(
                "INSERT INTO tasks VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
                params![task.id, serde_json::to_string(&task).map_err(fail)?],
            )
            .map_err(fail)?;
            remember_notion_writes(&tx, "task", &task.id, task.draft.write_to_notion)?;
            tx.execute("INSERT INTO operations VALUES(?1,?2)", params![op, input])
                .map_err(fail)?;
            tx.commit().map_err(fail)
        })
    }
    pub fn update_run(&self, run: &ScheduledRun) -> Result<(), EngineError> {
        self.write_run(run, None)
    }

    /// Record the transition and its follow-ups in one transaction. An action
    /// represents the latest outcome per task; archiving is per chat.
    pub(super) fn write_run(
        &self,
        run: &ScheduledRun,
        followup_at: Option<i64>,
    ) -> Result<(), EngineError> {
        blocking(|| {
            let mut c = self.0.lock().map_err(fail)?;
            let tx = c.transaction().map_err(fail)?;
            let body = serde_json::to_string(run).map_err(fail)?;
            tx.execute(
                "UPDATE runs SET body=?2,state=json_extract(?2,'$.state') WHERE id=?1",
                params![run.id, body],
            )
            .map_err(fail)?;
            remember_notion_writes(&tx, "run", &run.id, run.snapshot.write_to_notion)?;
            if let Some(at) = followup_at {
                let enqueue = |followup: Followup| {
                    let (kind, id) = (followup.kind(), followup.id(run));
                    tx.execute(
                        "INSERT INTO run_followups VALUES(?1,?2,?3,?4)
                         ON CONFLICT(kind,id) DO UPDATE SET body=excluded.body,due=excluded.due",
                        params![kind, id, body, at],
                    )
                    .map_err(fail)
                };
                enqueue(Followup::Action)?;
                if run.state == ScheduledRunState::Completed
                    && run.snapshot.output == ScheduledTaskOutput::Background
                    && run.snapshot.after_success == ScheduledTaskAfterSuccess::Archive
                {
                    enqueue(Followup::Archive)?;
                }
            }
            tx.commit().map_err(fail)
        })
    }

    pub(super) fn due_followups(
        &self,
        followup: Followup,
        now: i64,
    ) -> Result<Vec<ScheduledRun>, EngineError> {
        blocking(|| {
            let c = self.0.lock().map_err(fail)?;
            let mut q = c.prepare(
                "SELECT body FROM run_followups WHERE kind=?1 AND due<=?2 ORDER BY due,id LIMIT 32",
            ).map_err(fail)?;
            run_rows(&c, &mut q, params![followup.kind(), now])
        })
    }

    pub(super) fn finish_followup(
        &self,
        followup: Followup,
        run: &ScheduledRun,
        retry_at: Option<i64>,
    ) -> Result<(), EngineError> {
        blocking(|| {
            let c = self.0.lock().map_err(fail)?;
            let (kind, id) = (followup.kind(), followup.id(run));
            if let Some(at) = retry_at {
                c.execute("UPDATE run_followups SET due=?3 WHERE kind=?1 AND id=?2", params![kind, id, at])
            } else {
                c.execute("DELETE FROM run_followups WHERE kind=?1 AND id=?2", params![kind, id])
            }.map_err(fail)?;
            Ok(())
        })
    }
    pub fn claim(
        &self,
        task: &ScheduledTask,
        at: i64,
        now: i64,
        manual: Option<(&str, &str)>,
        skipped: Option<&str>,
    ) -> Result<(), EngineError> {
        blocking(|| {
            let mut c = self.0.lock().map_err(fail)?;
            let tx = c.transaction().map_err(fail)?;
            if let Some((op, input)) = manual {
                if replay(&tx, op, input)? {
                    return Ok(());
                }
            }
            let slot = manual
                .map(|(op, _)| format!("manual:{op}"))
                .unwrap_or_else(|| format!("scheduled:{at}"));
            let id = crate::work_outline_launch::scheduled_id(
                &format!("{}\0{}", task.device_id, task.id),
                &slot,
            );
            let run = ScheduledRun {
                id: id.clone(),
                task_id: task.id.clone(),
                scheduled_for: at,
                created_at: now,
                finished_at: skipped.map(|_| now),
                manual: manual.is_some(),
                state: if skipped.is_some() {
                    ScheduledRunState::Skipped
                } else {
                    ScheduledRunState::Launching
                },
                detail: skipped.map(str::to_owned),
                chat_id: skipped
                    .is_none()
                    .then(|| crate::work_outline_launch::scheduled_id(&id, "chat")),
                task_revision: task.revision,
                snapshot: task.draft.clone(),
                awaiting_request_id: None,
                awaiting_since: None,
            };
            tx.execute(
                "INSERT OR IGNORE INTO runs(id,task_id,slot,created_at,body,state) VALUES(?1,?2,?3,?4,?5,json_extract(?5,'$.state'))",
                params![
                    run.id,
                    task.id,
                    slot,
                    now,
                    serde_json::to_string(&run).map_err(fail)?
                ],
            )
            .map_err(fail)?;
            remember_notion_writes(&tx, "run", &run.id, run.snapshot.write_to_notion)?;
            if manual.is_none() {
                let mut task = task.clone();
                task.next_due = evaluator::next(&task.draft.schedule, now).map_err(fail)?;
                tx.execute(
                    "UPDATE tasks SET body=?2 WHERE id=?1",
                    params![task.id, serde_json::to_string(&task).map_err(fail)?],
                )
                .map_err(fail)?;
            }
            if let Some((op, input)) = manual {
                tx.execute("INSERT INTO operations VALUES(?1,?2)", params![op, input])
                    .map_err(fail)?;
            }
            tx.commit().map_err(fail)
        })
    }
}
/// The Write to Notion setting is also kept in a table that an older engine
/// neither reads nor rewrites. That engine drops the field from the JSON bodies
/// it saves, which would turn an explicit setting into "never set" (no Notion
/// instruction at all). Readers restore it when a body lacks it.
fn remember_notion_writes(
    c: &Connection,
    kind: &str,
    id: &str,
    allowed: Option<bool>,
) -> Result<(), EngineError> {
    if let Some(allowed) = allowed {
        c.execute(
            "INSERT INTO notion_writes VALUES(?1,?2,?3) ON CONFLICT(kind,id) DO UPDATE SET allowed=excluded.allowed",
            params![kind, id, allowed],
        )
        .map_err(fail)?;
    }
    Ok(())
}

fn restore_notion_writes(
    c: &Connection,
    kind: &str,
    id: &str,
    draft: &mut ScheduledTaskDraft,
) -> Result<(), EngineError> {
    if draft.write_to_notion.is_none() {
        draft.write_to_notion = c
            .query_row(
                "SELECT allowed FROM notion_writes WHERE kind=?1 AND id=?2",
                params![kind, id],
                |r| r.get(0),
            )
            .optional()
            .map_err(fail)?;
    }
    Ok(())
}

fn run_rows(
    c: &Connection,
    q: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> Result<Vec<ScheduledRun>, EngineError> {
    let runs: Vec<ScheduledRun> = q
        .query_map(params, |r| r.get::<_, String>(0))
        .map_err(fail)?
        .map(|r| serde_json::from_str(&r.map_err(fail)?).map_err(fail))
        .collect::<Result<_, _>>()?;
    runs.into_iter()
        .map(|mut run| {
            restore_notion_writes(c, "run", &run.id, &mut run.snapshot)?;
            Ok(run)
        })
        .collect()
}

fn replay(c: &Connection, op: &str, input: &str) -> Result<bool, EngineError> {
    if op.is_empty() || op.len() > 128 {
        return Err(fail("A 1–128 character operation ID is required"));
    }
    let previous: Option<String> = c
        .query_row("SELECT input FROM operations WHERE id=?1", [op], |r| {
            r.get(0)
        })
        .optional()
        .map_err(fail)?;
    match previous {
        Some(old) if old != input => Err(fail("Operation ID reused with different inputs")),
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task(root: &Path) -> ScheduledTask {
        ScheduledTask {
            id: "task".into(),
            revision: 1,
            device_id: "device".into(),
            draft: ScheduledTaskDraft {
                name: "Daily".into(),
                prompt: "Do the thing".into(),
                cwd: root.to_string_lossy().into(),
                space_id: None,
                config: ChatConfig { approval_rules: false,
                    harness: HarnessId::Mock,
                    model: Some("mock-1".into()),
                    reasoning: None,
                    model_options: Default::default(),
                    sandbox: SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                },
                schedule: TaskSchedule::Cron {
                    expression: "* * * * *".into(),
                    timezone: "UTC".into(),
                },
                enabled: true,
                grace_minutes: 360,
                cutover_confirmed: false,
                work_item_id: None,
                output: Default::default(),
                after_success: Default::default(),
                write_to_notion: None,
            },
            created_at: 0,
            updated_at: 0,
            next_due: Some(60_000),
            deleted: false,
            import_source: None,
            source_handoff_at: None,
        }
    }
    #[test]
    fn transactional_claim_survives_reopen_and_keeps_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let task = task(dir.path());
        store.save(task.clone(), 0, "save", "inputs").unwrap();
        store.claim(&task, 60_000, 60_000, None, None).unwrap();
        store.claim(&task, 60_000, 60_000, None, None).unwrap();
        assert_eq!(store.runs(None, None).unwrap().len(), 1);
        assert_eq!(store.tasks().unwrap()[0].next_due, Some(120_000));
        let mut edited = task;
        edited.revision = 2;
        edited.draft.name = "Renamed".into();
        store.save(edited, 1, "edit", "new inputs").unwrap();
        drop(store);
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.active().unwrap().len(), 1);
        assert_eq!(store.runs(None, None).unwrap()[0].snapshot.name, "Daily");
    }
    #[test]
    fn an_older_engine_rewrite_keeps_the_notion_setting() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let mut off = task(dir.path());
        off.draft.write_to_notion = Some(false);
        let mut on = task(dir.path());
        on.id = "on".into();
        on.draft.write_to_notion = Some(true);
        let legacy = {
            let mut legacy = task(dir.path());
            legacy.id = "legacy".into();
            legacy
        };
        for (i, t) in [&off, &on, &legacy].into_iter().enumerate() {
            store.save(t.clone(), 0, &format!("save-{i}"), "inputs").unwrap();
            store.claim(t, 60_000, 60_000, None, None).unwrap();
        }
        drop(store);
        // What a build without the field does: parse, drop it, write the bodies back.
        let c = Connection::open(dir.path().join("scheduled-tasks.sqlite3")).unwrap();
        c.execute_batch(
            "UPDATE tasks SET body=json_remove(body,'$.draft.writeToNotion');
             UPDATE runs SET body=json_remove(body,'$.snapshot.writeToNotion');",
        )
        .unwrap();
        assert_eq!(
            c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(),
            2
        );
        drop(c);
        let store = Store::open(dir.path()).unwrap();
        let by_task = |id: &str| {
            let task = store.tasks().unwrap().into_iter().find(|t| t.id == id).unwrap();
            let run = store.runs(Some(id), None).unwrap().remove(0);
            (task.draft.write_to_notion, run.snapshot.write_to_notion)
        };
        assert_eq!(by_task("task"), (Some(false), Some(false)));
        assert_eq!(by_task("on"), (Some(true), Some(true)));
        assert_eq!(by_task("legacy"), (None, None));
        assert_eq!(store.active().unwrap().iter().filter(|r| r.snapshot.write_to_notion == Some(false)).count(), 1);
    }
    #[test]
    fn a_rare_task_keeps_its_newest_run_beyond_the_history_page() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let mut rare = task(dir.path());
        rare.id = "rare".into();
        let busy = task(dir.path());
        store.save(rare.clone(), 0, "save-rare", "rare").unwrap();
        store.save(busy.clone(), 0, "save-busy", "busy").unwrap();
        store.claim(&rare, 1, 1, None, Some("missed")).unwrap();
        for at in 2..=120 {
            store.claim(&busy, at, at, None, Some("missed")).unwrap();
        }
        assert!(store.runs(None, None).unwrap().iter().all(|r| r.task_id == "task"));
        let latest = store.latest_per_task().unwrap();
        assert_eq!(latest.len(), 2);
        assert!(latest.iter().any(|r| r.task_id == "rare" && r.created_at == 1));
        assert!(latest.iter().any(|r| r.task_id == "task" && r.created_at == 120));
    }
    #[test]
    fn revisions_and_idempotency() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let task = task(dir.path());
        store.save(task.clone(), 0, "save", "inputs").unwrap();
        store.save(task.clone(), 0, "save", "inputs").unwrap();
        assert!(
            store
                .save(task.clone(), 0, "save", "different inputs")
                .is_err()
        );
        assert!(store.save(task.clone(), 0, "stale", "inputs").is_err());
        store
            .claim(&task, 0, 0, Some(("run", "manual")), None)
            .unwrap();
        store
            .claim(&task, 0, 0, Some(("run", "manual")), None)
            .unwrap();
        assert_eq!(store.runs(None, None).unwrap().len(), 1);
        assert_eq!(store.tasks().unwrap()[0].next_due, Some(60_000));
    }
    #[test]
    fn active_runs_are_not_history_limited() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let task = task(dir.path());
        for at in 0..105 {
            store.claim(&task, at, at, None, None).unwrap();
        }
        assert_eq!(store.runs(None, None).unwrap().len(), 100);
        assert_eq!(store.active().unwrap().len(), 105);
    }
    #[test]
    fn corrupt_database_is_not_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scheduled-tasks.sqlite3");
        std::fs::write(&path, b"broken database").unwrap();
        assert!(Store::open(dir.path()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"broken database");
    }
    #[test]
    fn migrates_legacy_run_ledger_without_replaying_it() {
        let dir = tempfile::tempdir().unwrap();
        let task = task(dir.path());
        let run = ScheduledRun {
            id: "run".into(),
            task_id: task.id.clone(),
            scheduled_for: 1,
            created_at: 1,
            finished_at: None,
            manual: false,
            state: ScheduledRunState::Queued,
            detail: None,
            chat_id: Some("chat".into()),
            task_revision: 1,
            snapshot: task.draft,
            awaiting_request_id: None,
            awaiting_since: None,
        };
        let c = Connection::open(dir.path().join("scheduled-tasks.sqlite3")).unwrap();
        c.execute_batch("CREATE TABLE runs(id TEXT PRIMARY KEY,task_id TEXT NOT NULL,slot TEXT NOT NULL,created_at INTEGER NOT NULL,body TEXT NOT NULL,UNIQUE(task_id,slot)); PRAGMA user_version=1;").unwrap();
        c.execute(
            "INSERT INTO runs VALUES('run','task','scheduled:1',1,?1)",
            [serde_json::to_string(&run).unwrap()],
        )
        .unwrap();
        drop(c);
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.active().unwrap().len(), 1);
        let mut done = run;
        done.state = ScheduledRunState::Completed;
        store.update_run(&done).unwrap();
        assert!(store.active().unwrap().is_empty());
        assert_eq!(store.runs(None, None).unwrap().len(), 1);
    }
}

/// SQLite fsync and its connection mutex must not pin a Tokio core thread.
pub(crate) fn blocking<T>(work: impl FnOnce() -> T) -> T {
    if tokio::runtime::Handle::try_current()
        .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
    {
        tokio::task::block_in_place(work)
    } else {
        work()
    }
}
