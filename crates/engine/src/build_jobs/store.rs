//! SQLite store for `build-jobs.sqlite3`. Schema documented in the module docs of
//! [`super`]; the pattern (user_version, WAL, `synchronous=FULL`, refuse a newer version) follows
//! `scheduled_tasks/store.rs`.

use super::{
    BuildJobError, Enqueued, Job, JobState, Lease, ListFilter, NewJob, Started, store_error,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

const SCHEMA_VERSION: i64 = 1;
const MAX_WAKE_ATTEMPTS: i64 = 20;

const COLUMNS: &str = "id, seq, chat_id, run_id, space_id, device_id, host, lane, worktree_key, cwd, branch, head_sha, \
    argv, env, label, integration, start_free_gib, timeout_secs, lease_id, state, reason, slot, created_at, started_at, \
    finished_at, pid, pgid, proc_start_time, remote_name, exit_code, signal, summary_json, log_path, stop_reason";

#[derive(Clone)]
pub struct Store(Arc<Mutex<Connection>>);

/// SQLite fsync and its connection mutex must not pin a Tokio core thread.
fn blocking<T>(work: impl FnOnce() -> T) -> T {
    if tokio::runtime::Handle::try_current()
        .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
    {
        tokio::task::block_in_place(work)
    } else {
        work()
    }
}

fn row_job(row: &Row<'_>) -> rusqlite::Result<Job> {
    row_job_at(row, 0)
}

/// Map the [`COLUMNS`] starting at column `o` (a join may put other columns first).
fn row_job_at(row: &Row<'_>, o: usize) -> rusqlite::Result<Job> {
    let json_err = |index: usize, error: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    let argv: String = row.get(o + 12)?;
    let env: String = row.get(o + 13)?;
    let state: String = row.get(o + 19)?;
    Ok(Job {
        id: row.get(o)?,
        seq: row.get(o + 1)?,
        chat_id: row.get(o + 2)?,
        run_id: row.get(o + 3)?,
        space_id: row.get(o + 4)?,
        device_id: row.get(o + 5)?,
        host: row.get(o + 6)?,
        lane: row.get(o + 7)?,
        worktree_key: row.get(o + 8)?,
        cwd: row.get(o + 9)?,
        branch: row.get(o + 10)?,
        head_sha: row.get(o + 11)?,
        argv: serde_json::from_str(&argv).map_err(|e| json_err(o + 12, e))?,
        env: serde_json::from_str(&env).map_err(|e| json_err(o + 13, e))?,
        label: row.get(o + 14)?,
        integration: row.get::<_, i64>(o + 15)? != 0,
        start_free_gib: row.get::<_, Option<i64>>(o + 16)?.map(|v| v as u64),
        timeout_secs: row.get::<_, i64>(o + 17)? as u64,
        lease_id: row.get(o + 18)?,
        state: JobState::parse(&state).unwrap_or(JobState::Lost),
        reason: row.get(o + 20)?,
        slot: row.get(o + 21)?,
        created_at: row.get(o + 22)?,
        started_at: row.get(o + 23)?,
        finished_at: row.get(o + 24)?,
        pid: row.get(o + 25)?,
        pgid: row.get(o + 26)?,
        proc_start_time: row.get(o + 27)?,
        remote_name: row.get(o + 28)?,
        exit_code: row.get(o + 29)?,
        signal: row.get(o + 30)?,
        summary_json: row.get(o + 31)?,
        log_path: row.get(o + 32)?,
        stop_reason: row.get(o + 33)?,
    })
}

/// [`COLUMNS`] prefixed with the `j.` alias.
fn qualified_columns() -> String {
    COLUMNS
        .split(',')
        .map(|c| format!("j.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn get_job(c: &Connection, id: &str) -> rusqlite::Result<Option<Job>> {
    c.query_row(
        &format!("SELECT {COLUMNS} FROM jobs WHERE id=?1"),
        [id],
        row_job,
    )
    .optional()
}

/// The identity of a command for idempotency: everything that changes what runs.
fn input_hash(new: &NewJob, host: &str) -> String {
    let identity = serde_json::json!([
        host,
        new.worktree_key,
        new.cwd,
        new.head_sha,
        new.argv,
        new.env,
        new.integration,
        new.lease_id
    ]);
    format!("{:x}", Sha256::digest(identity.to_string().as_bytes()))
}

impl Store {
    pub fn open(root: &Path) -> Result<Self, BuildJobError> {
        blocking(|| {
            let mut c = Connection::open(root.join("build-jobs.sqlite3")).map_err(store_error)?;
            let version: i64 = c
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .map_err(store_error)?;
            if version > SCHEMA_VERSION {
                return Err(BuildJobError::Store(
                    "This build queue database was written by a newer Mast version".into(),
                ));
            }
            c.pragma_update(None, "journal_mode", "WAL")
                .map_err(store_error)?;
            c.pragma_update(None, "synchronous", "FULL")
                .map_err(store_error)?;
            let tx = c.transaction().map_err(store_error)?;
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS jobs(
                   id TEXT PRIMARY KEY, seq INTEGER NOT NULL UNIQUE,
                   chat_id TEXT NOT NULL, run_id TEXT, space_id TEXT NOT NULL, device_id TEXT NOT NULL,
                   host TEXT NOT NULL, lane TEXT NOT NULL DEFAULT 'cargo-shared',
                   worktree_key TEXT NOT NULL, cwd TEXT NOT NULL, branch TEXT, head_sha TEXT,
                   argv TEXT NOT NULL, env TEXT NOT NULL, label TEXT,
                   integration INTEGER NOT NULL DEFAULT 0, start_free_gib INTEGER, timeout_secs INTEGER NOT NULL,
                   lease_id TEXT, state TEXT NOT NULL, reason TEXT, slot TEXT,
                   created_at INTEGER NOT NULL, started_at INTEGER, finished_at INTEGER,
                   pid INTEGER, pgid INTEGER, proc_start_time TEXT, remote_name TEXT,
                   exit_code INTEGER, signal INTEGER, summary_json TEXT, log_path TEXT NOT NULL, stop_reason TEXT);
                 CREATE INDEX IF NOT EXISTS jobs_queue ON jobs(host, state, seq);
                 CREATE INDEX IF NOT EXISTS jobs_chat ON jobs(chat_id, state);
                 CREATE UNIQUE INDEX IF NOT EXISTS jobs_slot_owner ON jobs(host, slot) WHERE state='running';
                 CREATE TABLE IF NOT EXISTS operations(op_id TEXT PRIMARY KEY, job_id TEXT NOT NULL, input_hash TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS waiters(
                   job_id TEXT NOT NULL, chat_id TEXT NOT NULL, wake INTEGER NOT NULL,
                   wake_state TEXT NOT NULL DEFAULT 'none', wake_attempts INTEGER NOT NULL DEFAULT 0,
                   PRIMARY KEY(job_id, chat_id));
                 CREATE INDEX IF NOT EXISTS waiters_pending ON waiters(wake_state, chat_id);
                 CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS quiet_windows(
                   host TEXT PRIMARY KEY, lease_id TEXT NOT NULL, owner TEXT NOT NULL,
                   requested_at INTEGER NOT NULL, granted_at INTEGER, expires_at INTEGER NOT NULL);
                 PRAGMA user_version=1;",
            )
            .map_err(store_error)?;
            tx.commit().map_err(store_error)?;
            Ok(Self(Arc::new(Mutex::new(c))))
        })
    }

    fn with<T>(
        &self,
        work: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, BuildJobError> {
        blocking(|| {
            let mut c = self.0.lock().map_err(store_error)?;
            work(&mut c).map_err(store_error)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &self,
        new: &NewJob,
        host: &str,
        id: &str,
        log_path: &str,
        timeout_secs: u64,
        now: i64,
        per_chat_limit: usize,
        per_space_limit: usize,
    ) -> Result<Enqueued, BuildJobError> {
        let hash = input_hash(new, host);
        blocking(|| {
            let mut c = self.0.lock().map_err(store_error)?;
            let tx = c.transaction().map_err(store_error)?;
            // Idempotent retry: the same key and command returns the same job.
            let prior: Option<(String, String)> = tx
                .query_row(
                    "SELECT job_id, input_hash FROM operations WHERE op_id=?1",
                    [&new.op_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(store_error)?;
            if let Some((job_id, prior_hash)) = prior {
                if prior_hash != hash {
                    return Err(BuildJobError::OperationConflict(new.op_id.clone()));
                }
                let job = get_job(&tx, &job_id)
                    .map_err(store_error)?
                    .ok_or_else(|| BuildJobError::NotFound(job_id))?;
                let position = position_of(&tx, &job).map_err(store_error)?;
                return Ok(Enqueued {
                    job,
                    position,
                    coalesced: false,
                    replay: true,
                });
            }
            let chat_active: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM waiters w JOIN jobs j ON j.id=w.job_id WHERE w.chat_id=?1 AND j.state IN ('awaiting_approval','queued','running')",
                    [&new.chat_id],
                    |r| r.get(0),
                )
                .map_err(store_error)?;
            if chat_active as usize >= per_chat_limit {
                return Err(BuildJobError::Limit(format!(
                    "a chat may have {per_chat_limit} unfinished build jobs"
                )));
            }
            let space_active: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM jobs WHERE space_id=?1 AND state IN ('awaiting_approval','queued','running')",
                    [&new.space_id],
                    |r| r.get(0),
                )
                .map_err(store_error)?;
            if space_active as usize >= per_space_limit {
                return Err(BuildJobError::Limit(format!(
                    "a Space may have {per_space_limit} unfinished build jobs"
                )));
            }
            let argv = serde_json::to_string(&new.argv).map_err(store_error)?;
            let env = serde_json::to_string(&new.env).map_err(store_error)?;
            // Coalescing: an identical *queued* job gains this chat as a waiter. An identical running
            // job does not, because the tree may have changed since it started.
            let twin: Option<String> = tx
                .query_row(
                    "SELECT id FROM jobs WHERE host=?1 AND state='queued' AND worktree_key=?2 AND cwd=?3 AND argv=?4 AND env=?5 \
                       AND IFNULL(head_sha,'')=?6 AND integration=?7 AND IFNULL(lease_id,'')=?8 ORDER BY seq LIMIT 1",
                    params![
                        host,
                        new.worktree_key,
                        new.cwd,
                        argv,
                        env,
                        new.head_sha.clone().unwrap_or_default(),
                        new.integration as i64,
                        new.lease_id.clone().unwrap_or_default()
                    ],
                    |r| r.get(0),
                )
                .optional()
                .map_err(store_error)?;
            let (job_id, coalesced) = match twin.filter(|_| !new.awaiting_approval) {
                Some(twin) => (twin, true),
                None => {
                    let seq: i64 = tx
                        .query_row("SELECT COALESCE(MAX(seq),0)+1 FROM jobs", [], |r| r.get(0))
                        .map_err(store_error)?;
                    let state = if new.awaiting_approval {
                        JobState::AwaitingApproval
                    } else {
                        JobState::Queued
                    };
                    tx.execute(
                        "INSERT INTO jobs(id, seq, chat_id, run_id, space_id, device_id, host, worktree_key, cwd, branch, head_sha, \
                           argv, env, label, integration, start_free_gib, timeout_secs, lease_id, state, created_at, log_path) \
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
                        params![
                            id,
                            seq,
                            new.chat_id,
                            new.run_id,
                            new.space_id,
                            new.device_id,
                            host,
                            new.worktree_key,
                            new.cwd,
                            new.branch,
                            new.head_sha,
                            argv,
                            env,
                            new.label,
                            new.integration as i64,
                            new.start_free_gib.map(|v| v as i64),
                            timeout_secs as i64,
                            new.lease_id,
                            state.as_str(),
                            now,
                            log_path
                        ],
                    )
                    .map_err(store_error)?;
                    (id.to_string(), false)
                }
            };
            tx.execute(
                "INSERT INTO waiters(job_id, chat_id, wake) VALUES(?1,?2,?3) \
                 ON CONFLICT(job_id, chat_id) DO UPDATE SET wake=MAX(wake, excluded.wake)",
                params![job_id, new.chat_id, new.wake as i64],
            )
            .map_err(store_error)?;
            tx.execute(
                "INSERT INTO operations(op_id, job_id, input_hash) VALUES(?1,?2,?3)",
                params![new.op_id, job_id, hash],
            )
            .map_err(store_error)?;
            let job = get_job(&tx, &job_id)
                .map_err(store_error)?
                .ok_or_else(|| BuildJobError::NotFound(job_id))?;
            let position = position_of(&tx, &job).map_err(store_error)?;
            tx.commit().map_err(store_error)?;
            Ok(Enqueued {
                job,
                position,
                coalesced,
                replay: false,
            })
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<Job>, BuildJobError> {
        self.with(|c| get_job(c, id))
    }

    pub fn list(&self, filter: &ListFilter) -> Result<Vec<Job>, BuildJobError> {
        self.with(|c| {
            let mut sql = format!("SELECT DISTINCT {} FROM jobs j", qualified_columns());
            let mut clauses = Vec::new();
            let mut args: Vec<String> = Vec::new();
            if let Some(chat) = &filter.chat_id {
                sql.push_str(" JOIN waiters w ON w.job_id=j.id");
                args.push(chat.clone());
                clauses.push(format!("w.chat_id=?{}", args.len()));
            }
            if let Some(space) = &filter.space_id {
                args.push(space.clone());
                clauses.push(format!("j.space_id=?{}", args.len()));
            }
            if filter.active_only {
                clauses.push("j.state IN ('awaiting_approval','queued','running')".into());
            }
            if !clauses.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&clauses.join(" AND "));
            }
            sql.push_str(" ORDER BY j.seq DESC");
            if let Some(limit) = filter.limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }
            let mut stmt = c.prepare(&sql)?;
            stmt.query_map(rusqlite::params_from_iter(args.iter()), row_job)?
                .collect()
        })
    }

    pub fn queued(&self, host: &str) -> Result<Vec<Job>, BuildJobError> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUMNS} FROM jobs WHERE host=?1 AND state='queued' ORDER BY seq"
            ))?;
            stmt.query_map([host], row_job)?.collect()
        })
    }

    pub fn running(&self, host: &str) -> Result<Vec<Job>, BuildJobError> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUMNS} FROM jobs WHERE host=?1 AND state='running' ORDER BY seq"
            ))?;
            stmt.query_map([host], row_job)?.collect()
        })
    }

    pub fn all_running(&self) -> Result<Vec<Job>, BuildJobError> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUMNS} FROM jobs WHERE state='running' ORDER BY seq"
            ))?;
            stmt.query_map([], row_job)?.collect()
        })
    }

    pub fn position(&self, id: &str) -> Result<usize, BuildJobError> {
        self.with(|c| match get_job(c, id)? {
            Some(job) => position_of(c, &job),
            None => Ok(0),
        })
    }

    pub fn active_count(&self, chat_id: &str) -> Result<usize, BuildJobError> {
        self.with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM waiters w JOIN jobs j ON j.id=w.job_id WHERE w.chat_id=?1 AND j.state IN ('awaiting_approval','queued','running')",
                [chat_id],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
        })
    }

    pub fn is_waiter(&self, job_id: &str, chat_id: &str) -> Result<bool, BuildJobError> {
        self.with(|c| {
            c.query_row(
                "SELECT 1 FROM waiters WHERE job_id=?1 AND chat_id=?2",
                [job_id, chat_id],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
        })
    }

    pub fn approve(&self, id: &str) -> Result<Job, BuildJobError> {
        let changed = self.with(|c| {
            c.execute(
                "UPDATE jobs SET state='queued' WHERE id=?1 AND state='awaiting_approval'",
                [id],
            )
        })?;
        let job = self
            .get(id)?
            .ok_or_else(|| BuildJobError::NotFound(id.into()))?;
        if changed == 0 {
            return Err(BuildJobError::Invalid(format!(
                "job {id} is {}, not awaiting approval",
                job.state.as_str()
            )));
        }
        Ok(job)
    }

    /// Cancel a job that never started; the waiters are woken.
    pub fn finish_unstarted(
        &self,
        id: &str,
        state: JobState,
        reason: &str,
        now: i64,
    ) -> Result<Job, BuildJobError> {
        self.with(|c| {
            let tx = c.transaction()?;
            let changed = tx.execute(
                "UPDATE jobs SET state=?2, reason=?3, finished_at=?4 WHERE id=?1 AND state IN ('awaiting_approval','queued')",
                params![id, state.as_str(), reason, now],
            )?;
            if changed > 0 {
                mark_pending(&tx, id)?;
            }
            tx.commit()
        })?;
        self.get(id)?
            .ok_or_else(|| BuildJobError::NotFound(id.into()))
    }

    pub fn cancel_queued_for_chat(
        &self,
        chat_id: &str,
        now: i64,
    ) -> Result<Vec<String>, BuildJobError> {
        self.with(|c| {
            let tx = c.transaction()?;
            // Only jobs this chat alone waits on: a coalesced job other chats still need keeps running.
            let ids: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT j.id FROM jobs j JOIN waiters w ON w.job_id=j.id WHERE w.chat_id=?1 \
                       AND j.state IN ('awaiting_approval','queued') \
                       AND (SELECT COUNT(*) FROM waiters x WHERE x.job_id=j.id)=1",
                )?;
                stmt.query_map([chat_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
            };
            for id in &ids {
                tx.execute(
                    "UPDATE jobs SET state='cancelled', reason='chat archived', finished_at=?2 WHERE id=?1",
                    params![id, now],
                )?;
            }
            tx.commit()?;
            Ok(ids)
        })
    }

    /// Claim the job's slot: `queued` -> `running` in one statement. `false` when the job is no
    /// longer queued or another running job owns the slot (the partial unique index).
    pub fn claim_running(
        &self,
        id: &str,
        slot: &str,
        remote_name: &str,
        now: i64,
    ) -> Result<bool, BuildJobError> {
        self.with(|c| {
            match c.execute(
                "UPDATE jobs SET state='running', slot=?2, remote_name=?3, started_at=?4, reason=NULL \
                 WHERE id=?1 AND state='queued'",
                params![id, slot, remote_name, now],
            ) {
                Ok(changed) => Ok(changed == 1),
                Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == rusqlite::ErrorCode::ConstraintViolation => Ok(false),
                Err(error) => Err(error),
            }
        })
    }

    pub fn unclaim(&self, id: &str, reason: &str) -> Result<(), BuildJobError> {
        self.with(|c| {
            c.execute(
                "UPDATE jobs SET state='queued', slot=NULL, remote_name=NULL, started_at=NULL, reason=?2 WHERE id=?1 AND state='running'",
                params![id, reason],
            )
            .map(|_| ())
        })
    }

    pub fn record_started(&self, id: &str, started: &Started) -> Result<(), BuildJobError> {
        self.with(|c| {
            c.execute(
                "UPDATE jobs SET pid=?2, pgid=?3, proc_start_time=?4, remote_name=COALESCE(?5, remote_name) WHERE id=?1",
                params![id, started.pid, started.pgid, started.proc_start_time, started.remote_name],
            )
            .map(|_| ())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish(
        &self,
        id: &str,
        state: JobState,
        exit_code: Option<i32>,
        signal: Option<i32>,
        reason: Option<&str>,
        summary_json: Option<&str>,
        now: i64,
    ) -> Result<(), BuildJobError> {
        self.with(|c| {
            let tx = c.transaction()?;
            let changed = tx.execute(
                "UPDATE jobs SET state=?2, exit_code=?3, signal=?4, reason=?5, summary_json=?6, finished_at=?7 \
                 WHERE id=?1 AND state IN ('queued','running')",
                params![id, state.as_str(), exit_code, signal, reason, summary_json, now],
            )?;
            if changed > 0 {
                mark_pending(&tx, id)?;
            }
            tx.commit()
        })
    }

    pub fn set_reason(&self, id: &str, reason: Option<&str>) -> Result<(), BuildJobError> {
        self.with(|c| {
            c.execute(
                "UPDATE jobs SET reason=?2 WHERE id=?1 AND state='running'",
                params![id, reason],
            )
            .map(|_| ())
        })
    }

    pub fn set_stop_reason(&self, id: &str, reason: &str) -> Result<(), BuildJobError> {
        self.with(|c| {
            c.execute("UPDATE jobs SET stop_reason=?2 WHERE id=?1 AND state='running' AND stop_reason IS NULL", params![id, reason])
                .map(|_| ())
        })
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>, BuildJobError> {
        self.with(|c| {
            c.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
                .optional()
        })
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), BuildJobError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO meta VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )
            .map(|_| ())
        })
    }

    /// Terminal jobs with an undelivered wake, grouped by waiting chat (oldest completion first).
    pub fn pending_wakes(&self) -> Result<Vec<(String, Vec<Job>)>, BuildJobError> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT w.chat_id, {} FROM waiters w JOIN jobs j ON j.id=w.job_id WHERE w.wake_state='pending' ORDER BY w.chat_id, j.finished_at, j.seq",
                qualified_columns()
            ))?;
            let rows = stmt.query_map([], |r| {
                let chat: String = r.get(0)?;
                Ok((chat, row_job_at(r, 1)?))
            })?;
            let mut out: Vec<(String, Vec<Job>)> = Vec::new();
            for row in rows {
                let (chat, job) = row?;
                match out.last_mut() {
                    Some((last, jobs)) if *last == chat => jobs.push(job),
                    _ => out.push((chat, vec![job])),
                }
            }
            Ok(out)
        })
    }

    pub fn mark_wakes(
        &self,
        chat_id: &str,
        job_ids: &[&str],
        state: &str,
    ) -> Result<(), BuildJobError> {
        self.with(|c| {
            let tx = c.transaction()?;
            for id in job_ids {
                tx.execute(
                    "UPDATE waiters SET wake_state=?3 WHERE job_id=?1 AND chat_id=?2 AND wake_state='pending'",
                    params![id, chat_id, state],
                )?;
            }
            tx.commit()
        })
    }

    /// A failed delivery stays pending; after [`MAX_WAKE_ATTEMPTS`] it is given up as `failed`.
    pub fn bump_wake_attempts(&self, chat_id: &str, job_ids: &[&str]) -> Result<(), BuildJobError> {
        self.with(|c| {
            let tx = c.transaction()?;
            for id in job_ids {
                tx.execute(
                    "UPDATE waiters SET wake_attempts=wake_attempts+1, \
                       wake_state=CASE WHEN wake_attempts+1>=?3 THEN 'failed' ELSE wake_state END \
                     WHERE job_id=?1 AND chat_id=?2 AND wake_state='pending'",
                    params![id, chat_id, MAX_WAKE_ATTEMPTS],
                )?;
            }
            tx.commit()
        })
    }

    // Quiet-window lease ------------------------------------------------------------------------

    pub fn acquire_lease(
        &self,
        host: &str,
        owner: &str,
        now: i64,
        expires_at: i64,
    ) -> Result<Lease, BuildJobError> {
        self.expire_leases(now)?;
        let existing = self.lease(host, now)?;
        match existing {
            Some(lease) if lease.owner == owner => Ok(lease),
            Some(lease) => Err(BuildJobError::Invalid(format!(
                "quiet window on {host} is held by {}",
                lease.owner
            ))),
            None => {
                let lease_id = crate::new_id();
                self.with(|c| {
                    c.execute(
                        "INSERT INTO quiet_windows(host, lease_id, owner, requested_at, expires_at) VALUES(?1,?2,?3,?4,?5)",
                        params![host, lease_id, owner, now, expires_at],
                    )
                    .map(|_| ())
                })?;
                Ok(Lease {
                    host: host.into(),
                    lease_id,
                    owner: owner.into(),
                    granted: false,
                    expires_at,
                })
            }
        }
    }

    pub fn lease(&self, host: &str, now: i64) -> Result<Option<Lease>, BuildJobError> {
        self.with(|c| {
            c.query_row(
                "SELECT lease_id, owner, granted_at IS NOT NULL, expires_at FROM quiet_windows WHERE host=?1 AND expires_at>?2",
                params![host, now],
                |r| Ok(Lease { host: host.into(), lease_id: r.get(0)?, owner: r.get(1)?, granted: r.get(2)?, expires_at: r.get(3)? }),
            )
            .optional()
        })
    }

    /// Mark the host's lease granted. The caller has checked that nothing runs on the host.
    pub fn grant_lease(&self, host: &str) -> Result<(), BuildJobError> {
        self.with(|c| {
            c.execute(
                "UPDATE quiet_windows SET granted_at=COALESCE(granted_at, ?2) WHERE host=?1",
                params![host, crate::now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn expire_leases(&self, now: i64) -> Result<(), BuildJobError> {
        self.with(|c| {
            c.execute("DELETE FROM quiet_windows WHERE expires_at<=?1", [now])
                .map(|_| ())
        })
    }

    pub fn release_lease(&self, host: &str, lease_id: &str) -> Result<bool, BuildJobError> {
        self.with(|c| {
            c.execute(
                "DELETE FROM quiet_windows WHERE host=?1 AND lease_id=?2",
                params![host, lease_id],
            )
            .map(|n| n > 0)
        })
    }
}

/// Every waiter of a job that asked to be woken becomes `pending`.
fn mark_pending(c: &Connection, job_id: &str) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE waiters SET wake_state='pending' WHERE job_id=?1 AND wake=1 AND wake_state='none'",
        [job_id],
    )?;
    Ok(())
}

fn position_of(c: &Connection, job: &Job) -> rusqlite::Result<usize> {
    if job.state != JobState::Queued {
        return Ok(0);
    }
    c.query_row(
        "SELECT COUNT(*) FROM jobs WHERE host=?1 AND state='queued' AND seq<=?2",
        params![job.host, job.seq],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n as usize)
}
