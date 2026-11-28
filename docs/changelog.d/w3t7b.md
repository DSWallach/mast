# Worker continuation, stage 2

- `whoami` and `get_chat` show a `remoteParent` (`{peerId, chatId}`) and a note for a chat a paired device created on this engine. Its `parentChatId` is null by design (the parent lives on the other device); `report_to_parent` still works and reaches that chat, and chats the coordinator starts here report to it locally. A new `GetRemoteParent` RPC serves it.
- A credit-limit successor, and a stopped worker continued by a launch, are titled "<source title> (continued)" so they are told apart from the chat they continue. A hand-off from the dialog or the tool keeps the source's title, as before.
- Not done yet: the Hand off dialog's success notice and an "Open successor" entry for an archived source.
