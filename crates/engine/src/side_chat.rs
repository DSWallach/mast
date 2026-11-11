//! "Absorb into main chat": hand a side chat's new exchanges to its main
//! chat as context.

use mast_proto::side_chat::{AbsorbSideChatReply, SideChatDelivery, absorb_message};

use crate::EngineError;
use crate::doc_host::DocHost;

/// An absorb row still waiting in the main chat's queue: what it covers,
/// so deleting it moves the marker back and a new absorb extends it.
#[derive(Debug, Clone)]
pub(crate) struct PendingAbsorb {
    pub main_id: String,
    pub row_id: String,
    /// The marker before this row (where its exchanges start).
    pub from: Option<String>,
    pub through: String,
}

impl DocHost {
    /// Queue the side chat's not-yet-absorbed exchanges for its main chat
    /// (an idle main chat takes them at once; otherwise they wait in its
    /// queue) and advance the absorbed-through marker. Never steers or
    /// interrupts. While an earlier absorb is still queued, its row is
    /// extended instead of adding a second one.
    pub async fn absorb_side_chat(&self, side_id: &str) -> Result<AbsorbSideChatReply, EngineError> {
        let workspace = self
            .workspace()
            .ok_or_else(|| EngineError::Other("workspace unavailable".into()))?;
        let handle = self.open(side_id)?;
        let _absorbing = handle.absorb_lock.lock().await;
        let side = workspace
            .chat(side_id)?
            .ok_or_else(|| EngineError::Other(format!("no such chat: {side_id}")))?;
        let main_id = side
            .side_of
            .clone()
            .ok_or_else(|| EngineError::Other("not a side chat".into()))?;
        let sessions = self.sessions();
        if sessions.as_ref().is_some_and(|s| s.turn_in_flight(side_id)) {
            return Err(EngineError::Other("the side chat is still answering".into()));
        }
        let main = self.open(&main_id)?;
        // Nothing takes from or removes the main queue while this runs.
        let _drain = main.drain_lock.lock().await;
        let queued = main.doc().read_queue()?;
        let pending = self
            .side_absorbs()
            .get(side_id)
            .filter(|p| p.main_id == main_id && queued.iter().any(|row| row.id == p.row_id))
            .cloned();
        let from = match &pending {
            Some(pending) => pending.from.clone(),
            None => side.side_absorbed_through.clone(),
        };
        let entries = handle.doc().read_entries()?;
        let batch = mast_doc::side_chat::absorb_batch(&entries, from.as_deref())
            .filter(|batch| pending.as_ref().is_none_or(|p| p.through != batch.through))
            .ok_or_else(|| EngineError::Other("nothing new to absorb".into()))?;
        let text = absorb_message(&batch.exchanges);
        let waits = pending.is_some()
            || !queued.is_empty()
            || main.queue_paused.load(std::sync::atomic::Ordering::Acquire)
            || sessions.is_some_and(|s| s.turn_in_flight(&main_id));
        let row_id = match pending {
            Some(pending) => {
                main.doc()
                    .set_queued_text(&pending.row_id, &text, chrono::Utc::now().timestamp_millis())?;
                main.publish_queue();
                pending.row_id
            }
            None => {
                let row_id = uuid::Uuid::new_v4().to_string();
                self.queue_context_message(&main_id, &row_id, &text)?
            }
        };
        workspace.set_chat_side_absorbed_through(side_id, Some(&batch.through))?;
        self.side_absorbs().insert(
            side_id.to_owned(),
            PendingAbsorb { main_id, row_id, from, through: batch.through.clone() },
        );
        Ok(AbsorbSideChatReply {
            delivery: if waits { SideChatDelivery::Queue } else { SideChatDelivery::Run },
            exchanges: batch.exchanges.len(),
            absorbed_through: batch.through,
        })
    }

    /// A queued absorb row was deleted before delivery: its exchanges count
    /// as not absorbed again.
    pub(crate) fn forget_side_absorb_row(&self, main_id: &str, row_id: &str) {
        let removed = {
            let mut pending = self.side_absorbs();
            let side_id = pending
                .iter()
                .find(|(_, p)| p.main_id == main_id && p.row_id == row_id)
                .map(|(side_id, _)| side_id.clone());
            side_id.and_then(|side_id| pending.remove(&side_id).map(|p| (side_id, p)))
        };
        let (Some((side_id, pending)), Some(workspace)) = (removed, self.workspace()) else {
            return;
        };
        let marker = workspace.chat(&side_id).ok().flatten().and_then(|chat| chat.side_absorbed_through);
        if marker.as_deref() == Some(pending.through.as_str())
            && let Err(err) = workspace.set_chat_side_absorbed_through(&side_id, pending.from.as_deref())
        {
            tracing::warn!(chat = %side_id, error = %err, "moving the absorbed marker back failed");
        }
    }
}

impl DocHost {
    /// An archived side chat stops for good: its queue is dropped (so no
    /// turn end drains it) and any running turn is interrupted.
    pub async fn retire_side_chat(&self, chat_id: &str, open: bool) -> Result<(), EngineError> {
        let handle = if open { Some(self.open(chat_id)?) } else { self.loaded(chat_id) };
        if let Some(handle) = handle {
            let _drain = handle.drain_lock.lock().await;
            let doc = handle.doc();
            let rows = doc.read_queue()?;
            for row in &rows {
                doc.take_queued(&row.id)?;
            }
            if !rows.is_empty() {
                handle.publish_queue();
            }
        }
        if let Some(sessions) = self.sessions() {
            sessions.interrupt(chat_id).await?;
        }
        Ok(())
    }

    /// Boot: side chats live only in a pane, and panes don't survive a
    /// restart, so every unarchived side chat this device hosts is archived
    /// and retired.
    pub fn archive_orphaned_side_chats(&self) -> usize {
        let Some(workspace) = self.workspace() else {
            return 0;
        };
        let chats = match workspace.read_chats() {
            Ok(chats) => chats,
            Err(err) => {
                tracing::warn!(error = %err, "side chat boot sweep: reading chats failed");
                return 0;
            }
        };
        let mut archived = 0;
        for chat in chats.iter().filter(|chat| {
            chat.is_side_chat() && !chat.archived && chat.device_id == workspace.device_id()
        }) {
            match workspace.set_chat_archived(&chat.id, true) {
                Ok(_) => archived += 1,
                Err(err) => {
                    tracing::warn!(chat = %chat.id, error = %err, "archiving an orphaned side chat failed");
                    continue;
                }
            }
            let host = self.clone();
            let chat_id = chat.id.clone();
            tokio::spawn(async move {
                if let Err(err) = host.retire_side_chat(&chat_id, true).await {
                    tracing::warn!(chat = %chat_id, error = %err, "retiring an orphaned side chat failed");
                }
            });
        }
        archived
    }

    /// Retire every side chat this device hosts as soon as it is archived,
    /// whichever path (UI, agent tool, another device) archived it.
    pub fn spawn_side_chat_reaper(&self) {
        let Some(workspace) = self.workspace().cloned() else {
            return;
        };
        let host = self.clone();
        let mut chats = workspace.watch_chats();
        tokio::spawn(async move {
            let mut retired = std::collections::HashSet::<String>::new();
            loop {
                let due: Vec<String> = {
                    let chats = chats.borrow_and_update();
                    retired.retain(|id| chats.iter().any(|chat| &chat.id == id && chat.archived));
                    chats
                        .iter()
                        .filter(|chat| {
                            chat.is_side_chat()
                                && chat.archived
                                && chat.device_id == workspace.device_id()
                                && !retired.contains(&chat.id)
                        })
                        .map(|chat| chat.id.clone())
                        .collect()
                };
                for chat_id in due {
                    if let Err(err) = host.retire_side_chat(&chat_id, false).await {
                        tracing::warn!(chat = %chat_id, error = %err, "retiring an archived side chat failed");
                    }
                    retired.insert(chat_id);
                }
                if chats.changed().await.is_err() {
                    return;
                }
            }
        });
    }
}
