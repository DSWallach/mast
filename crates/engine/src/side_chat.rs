//! "Absorb into main chat": hand a side chat's new exchanges to its main
//! chat as context.

use mast_proto::side_chat::{AbsorbSideChatReply, SideChatDelivery, absorb_message};

use crate::EngineError;
use crate::doc_host::DocHost;

impl DocHost {
    /// Queue the side chat's not-yet-absorbed exchanges for its main chat
    /// (an idle main chat takes them at once; a busy one after its turn)
    /// and advance the absorbed-through marker. Never steers or interrupts.
    pub async fn absorb_side_chat(
        &self,
        side_id: &str,
    ) -> Result<AbsorbSideChatReply, EngineError> {
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
            .ok_or_else(|| EngineError::Other("not a side chat".into()))?;
        let sessions = self.sessions();
        if sessions.as_ref().is_some_and(|s| s.turn_in_flight(side_id)) {
            return Err(EngineError::Other(
                "the side chat is still answering".into(),
            ));
        }
        let entries = handle.doc().read_entries()?;
        let batch =
            mast_doc::side_chat::absorb_batch(&entries, side.side_absorbed_through.as_deref())
                .ok_or_else(|| EngineError::Other("nothing new to absorb".into()))?;
        let delivery = if sessions.is_some_and(|s| s.turn_in_flight(&main_id)) {
            SideChatDelivery::Queue
        } else {
            SideChatDelivery::Run
        };
        self.queue_context_message(&main_id, &absorb_message(&batch.exchanges))?;
        workspace.set_chat_side_absorbed_through(side_id, &batch.through)?;
        Ok(AbsorbSideChatReply {
            delivery,
            exchanges: batch.exchanges.len(),
            absorbed_through: batch.through,
        })
    }
}
