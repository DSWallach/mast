# Worker continuation, stage 3: Open successor

- A chat the Hand off dialog archived has an "Open successor" row in its chat menu and its outline row menu. Chats now carry `handOffFrom` (the chat they continue), read from the hand-off record the engine already writes, so a view can find a chat's successor.
- The Hand off dialog confirms with a small notice ("Handed off to a new chat", with an Open action) that dismisses itself or on click. The notice is a reusable Shell-level strip.
- A reply lost on its way back no longer shows "failed" when the successor was made: the dialog checks for the chat its operation derives and closes as a success.
