//! SQLite store for `merge-queue.sqlite3`. The pattern (user_version, WAL,
//! `synchronous=FULL`, refuse a newer version) follows `build_jobs/store.rs`.
//!
//! # Schema (version 1)
//!
//! ```text
//! requests(seq PK AUTOINCREMENT, id UNIQUE, repo, target_ref, work_item_id, tested_sha, state,
//!          data JSON)                              -- the whole LandRequest
//! landings(request_id PK, work_item_id, landed_at, data JSON)   -- the whole LandedRecord
//! ```
//!
//! A partial unique index allows one `building` or `awaiting_review` row per
//! `(repo, target_ref)`: "one land at a time per target" is a database
//! invariant, not only a pipeline rule. The columns beside `data` exist for the
//! queries and are written from the same value.

use super::{LandRequest, LandedRecord, MergeQueueError};
use rusqlite::{Connection, OptionalExtension, params};
use serde::de::DeserializeOwned;
use std::{path::Path, sync::Mutex};

const SCHEMA_VERSION: i64 = 1;

fn store_error(error: impl std::fmt::Display) -> MergeQueueError {
    MergeQueueError::Store(error.to_string())
}

pub(super) struct Store(Mutex<Connection>);

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

fn decode<T: DeserializeOwned>(text: String) -> Result<T, MergeQueueError> {
    serde_json::from_str(&text).map_err(store_error)
}

impl Store {
    pub fn open(root: &Path) -> Result<Self, MergeQueueError> {
        blocking(|| {
            let mut c = Connection::open(root.join("merge-queue.sqlite3")).map_err(store_error)?;
            let version: i64 = c
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .map_err(store_error)?;
            if version > SCHEMA_VERSION {
                return Err(MergeQueueError::Store(
                    "This merge queue database was written by a newer Mast version".into(),
                ));
            }
            c.pragma_update(None, "journal_mode", "WAL")
                .map_err(store_error)?;
            c.pragma_update(None, "synchronous", "FULL")
                .map_err(store_error)?;
            let tx = c.transaction().map_err(store_error)?;
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS requests(
                   seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
                   repo TEXT NOT NULL, target_ref TEXT NOT NULL, work_item_id TEXT NOT NULL,
                   tested_sha TEXT NOT NULL, state TEXT NOT NULL, data TEXT NOT NULL);
                 CREATE UNIQUE INDEX IF NOT EXISTS requests_lane_owner ON requests(repo, target_ref)
                   WHERE state IN ('building','awaiting_review');
                 CREATE INDEX IF NOT EXISTS requests_lane ON requests(repo, target_ref, state, seq);
                 CREATE INDEX IF NOT EXISTS requests_item ON requests(work_item_id, seq);
                 CREATE TABLE IF NOT EXISTS landings(
                   request_id TEXT PRIMARY KEY, work_item_id TEXT NOT NULL,
                   landed_at INTEGER NOT NULL, data TEXT NOT NULL);
                 CREATE INDEX IF NOT EXISTS landings_item ON landings(work_item_id, landed_at);
                 PRAGMA user_version=1;",
            )
            .map_err(store_error)?;
            tx.commit().map_err(store_error)?;
            Ok(Self(Mutex::new(c)))
        })
    }

    fn with<T>(
        &self,
        work: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, MergeQueueError> {
        blocking(|| {
            let mut c = self.0.lock().map_err(store_error)?;
            work(&mut c).map_err(store_error)
        })
    }

    /// Insert a new request and give it its queue position.
    pub fn insert(&self, request: &mut LandRequest) -> Result<(), MergeQueueError> {
        let mut value = serde_json::to_value(&*request).map_err(store_error)?;
        let seq = self.with(|c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO requests(id, repo, target_ref, work_item_id, tested_sha, state, data)
                 VALUES(?1,?2,?3,?4,?5,?6,'')",
                params![
                    request.id,
                    request.repo,
                    request.target_ref,
                    request.work_item_id,
                    request.tested_sha,
                    request.state.as_str()
                ],
            )?;
            let seq = tx.last_insert_rowid();
            value["seq"] = seq.into();
            tx.execute(
                "UPDATE requests SET data=?2 WHERE seq=?1",
                params![seq, value.to_string()],
            )?;
            tx.commit()?;
            Ok(seq)
        })?;
        request.seq = seq;
        Ok(())
    }

    /// Whether anything has landed on `target_ref`.
    pub fn has_landed(&self, repo: &str, target_ref: &str) -> Result<bool, MergeQueueError> {
        self.with(|c| {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM requests
                   WHERE repo=?1 AND target_ref=?2 AND state='landed')",
                [repo, target_ref],
                |r| r.get(0),
            )
        })
    }

    pub fn save(&self, request: &LandRequest) -> Result<(), MergeQueueError> {
        let data = serde_json::to_string(request).map_err(store_error)?;
        self.with(|c| {
            c.execute(
                "UPDATE requests SET state=?2, data=?3 WHERE id=?1",
                params![request.id, request.state.as_str(), data],
            )
            .map(|_| ())
        })
    }

    /// Save a landed request and its landing record in one transaction, so the
    /// landed signal never disagrees with the request.
    pub fn save_landed(
        &self,
        request: &LandRequest,
        landing: &LandedRecord,
    ) -> Result<(), MergeQueueError> {
        let data = serde_json::to_string(request).map_err(store_error)?;
        let record = serde_json::to_string(landing).map_err(store_error)?;
        self.with(|c| {
            let tx = c.transaction()?;
            tx.execute(
                "UPDATE requests SET state=?2, data=?3 WHERE id=?1",
                params![request.id, request.state.as_str(), data],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO landings(request_id, work_item_id, landed_at, data)
                 VALUES(?1,?2,?3,?4)",
                params![
                    landing.request_id,
                    landing.work_item_id,
                    landing.landed_at,
                    record
                ],
            )?;
            tx.commit()
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<LandRequest>, MergeQueueError> {
        self.with(|c| {
            c.query_row("SELECT data FROM requests WHERE id=?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .optional()
        })?
        .map(decode)
        .transpose()
    }

    fn rows<T: DeserializeOwned>(
        &self,
        sql: &str,
        values: &[&str],
    ) -> Result<Vec<T>, MergeQueueError> {
        let texts = self.with(|c| {
            let mut statement = c.prepare(sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(values), |r| {
                r.get::<_, String>(0)
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })?;
        texts.into_iter().map(decode).collect()
    }

    /// Every request, oldest first.
    pub fn all(&self) -> Result<Vec<LandRequest>, MergeQueueError> {
        self.rows("SELECT data FROM requests ORDER BY seq", &[])
    }

    /// The newest request with the same lane, work item and tested commit.
    pub fn latest_matching(
        &self,
        repo: &str,
        target_ref: &str,
        work_item_id: &str,
        tested_sha: &str,
    ) -> Result<Option<LandRequest>, MergeQueueError> {
        Ok(self
            .rows(
                "SELECT data FROM requests WHERE repo=?1 AND target_ref=?2 AND work_item_id=?3
                   AND tested_sha=?4 ORDER BY seq DESC LIMIT 1",
                &[repo, target_ref, work_item_id, tested_sha],
            )?
            .pop())
    }

    /// The request a lane works on now: the active one, else the oldest queued.
    pub fn lane_head(
        &self,
        repo: &str,
        target_ref: &str,
    ) -> Result<Option<LandRequest>, MergeQueueError> {
        Ok(self
            .rows(
                "SELECT data FROM requests WHERE repo=?1 AND target_ref=?2
                   AND state IN ('queued','building','awaiting_review')
                 ORDER BY (state='queued'), seq LIMIT 1",
                &[repo, target_ref],
            )?
            .pop())
    }

    /// Every `(repo, target_ref)` with unfinished requests, oldest work first.
    pub fn open_lanes(&self) -> Result<Vec<(String, String)>, MergeQueueError> {
        self.with(|c| {
            let mut statement = c.prepare(
                "SELECT repo, target_ref FROM requests
                  WHERE state IN ('queued','building','awaiting_review')
                  GROUP BY repo, target_ref ORDER BY MIN(seq)",
            )?;
            let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })
    }

    /// Landings of one work item, newest first.
    pub fn landings(&self, work_item_id: &str) -> Result<Vec<LandedRecord>, MergeQueueError> {
        self.rows(
            "SELECT data FROM landings WHERE work_item_id=?1 ORDER BY landed_at DESC, rowid DESC",
            &[work_item_id],
        )
    }
}
