use super::*;

pub(super) fn forward_deadline(method: &str) -> std::time::Duration {
    use std::time::Duration;
    match method {
        methods::CLONE_REPO | methods::FETCH_ALL | methods::APPLY_UPDATE => {
            Duration::from_secs(15 * 60)
        }
        methods::INSTALL_HARNESS => Duration::from_secs(15 * 60),
        methods::CREATE_WORKTREE => Duration::from_secs(120),
        // Allow the adapter discovery budget plus relay and shutdown overhead.
        methods::LIST_MODELS | methods::LIST_COMMANDS => Duration::from_secs(100),
        // Validation plus an optional `git worktree add` on the owner.
        methods::CREATE_CHAT_FOR_WORK_ITEM => Duration::from_secs(120),
        // `gh pr view`, a fetch, and `gh pr checkout` / `git worktree add`.
        methods::START_PR_REVIEW | methods::START_BRANCH_REVIEW => Duration::from_secs(180),
        _ => Duration::from_secs(30),
    }
}

/// ControlRpc methods that honor `targetDeviceId` (feature-inventory §2.1). Extend this
/// list (plus [`is_stream_method`] for streams) to make more of the surface
/// device-addressable — the handlers themselves need no changes.
pub(super) fn forwardable(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_HARNESSES
            | methods::INSTALL_HARNESS
            | methods::GET_TITLE_SETTINGS
            | methods::SET_TITLE_SETTINGS
            | methods::SET_HARNESS_ENABLED
            | methods::LIST_MODELS
            | methods::LIST_COMMANDS
            | methods::QUEUE_COMMAND
            | methods::RETRY_ERRORED_TURN
            | methods::TAKE_PROJECT_ACTION_SETUP
            | methods::WATCH_DOC_MESSAGES
            // The queue lives on the chat doc, and only its host may send from
            // it — same addressing as the command ledger next door.
            | methods::WATCH_QUEUE
            | methods::QUEUE_MESSAGE
            | methods::UPDATE_QUEUED_MESSAGE
            | methods::BEGIN_QUEUED_MESSAGE_EDIT
            | methods::RENEW_QUEUED_MESSAGE_EDIT
            | methods::FINISH_QUEUED_MESSAGE_EDIT
            | methods::MOVE_QUEUED_MESSAGE
            | methods::REMOVE_QUEUED_MESSAGE
            | methods::SEND_QUEUED_MESSAGE_NOW
            | methods::STEER_QUEUED_MESSAGE_NOW
            // Repos/worktrees/folders are device-local filesystem state.
            | methods::LIST_REPOS
            | methods::ADD_REPO
            | methods::CLONE_REPO
            | methods::CREATE_REPO
            | methods::LIST_BRANCHES
            | methods::LIST_REFS
            | methods::LIST_GIT_HISTORY
            | methods::SEARCH_GIT_HISTORY
            | methods::RESOLVE_GIT_AVATARS
            | methods::FETCH_ALL
            | methods::SWITCH_REF
            | methods::LIST_FOLDERS
            | methods::LIST_DRIVES
            | methods::SEARCH_FILES
            | methods::LIST_WORKSPACE_DIRECTORY
            | methods::SEARCH_WORKSPACE_FILES
            | methods::WATCH_WORKSPACE_SEARCH_FILES
            | methods::WATCH_WORKSPACE_SYMBOLS
            | methods::REFRESH_WORKSPACE_SYMBOLS
            | methods::LIST_WORKSPACE_FILE_HISTORY
            | methods::GET_WORKSPACE_FILE_HISTORY_DIFF
            | methods::SEARCH_WORKSPACE_CONTENT
            | methods::READ_WORKSPACE_IMAGE
            | methods::READ_WORKSPACE_FILE
            | methods::WRITE_WORKSPACE_FILE
            | methods::WATCH_WORKSPACE_FILES
            | methods::CREATE_WORKTREE
            | methods::DELETE_WORKTREE
            // Project Actions live in the owning engine's private profile store.
            | methods::LIST_PROJECT_ACTIONS
            | methods::UPSERT_PROJECT_ACTION
            | methods::DELETE_PROJECT_ACTION
            | methods::RUN_PROJECT_ACTION
            // Checkout diffs are produced on the device holding the checkout.
            | methods::WATCH_CHECKOUT_DIFFS
            | methods::WATCH_WORKSPACE_GIT_STATUS
            | methods::WATCH_CHECKOUT_CHANGE_REQUEST
            | methods::GET_CHECKOUT_DIFF
            | methods::RESOLVE_CODE_REF_SOURCE
            | methods::RESOLVE_BRANCH_REVIEW_BASE
            | methods::GET_CHECKOUT_FILE_DIFF_TEXT
            // Terminals live on the chat's host device.
            | methods::OPEN_TERMINAL
            | methods::SUBSCRIBE_TERMINAL
            | methods::WRITE_TERMINAL
            | methods::RESIZE_TERMINAL
            | methods::CLOSE_TERMINAL
            // Agent accounts are per-device CLI logins (the device switcher
            // retargets which device's logins are shown).
            | methods::LIST_AGENT_ACCOUNTS
            | methods::LOCAL_AGENT_SIGN_IN_STATUS
            | methods::ACTIVATE_AGENT_ACCOUNT
            | methods::FORGET_AGENT_ACCOUNT
            | methods::START_AGENT_LOGIN
            | methods::COMPLETE_AGENT_LOGIN
            | methods::POLL_AGENT_LOGIN
            | methods::CANCEL_AGENT_LOGIN
            | methods::LIST_MANAGED_ACCOUNTS
            | methods::CREATE_MANAGED_ACCOUNT
            | methods::REMOVE_MANAGED_ACCOUNT
            | methods::START_MANAGED_ACCOUNT_LOGIN
            | methods::POLL_MANAGED_ACCOUNT_LOGIN
            | methods::CANCEL_MANAGED_ACCOUNT_LOGIN
            | methods::SUBMIT_MANAGED_ACCOUNT_LOGIN_CODE
            | methods::PROMOTE_SAVED_ACCOUNT
            | methods::SET_PROJECT_ACCOUNT
            // Uploads/attachments target the chat's host device (the agent reads
            // the committed file from that device's disk).
            | methods::UPLOAD_CHUNK
            | methods::UPLOAD_COMMIT
            | methods::READ_ATTACHMENT_CHUNK
            // Updates report/apply on the device whose binary they concern.
            | methods::UPDATE_STATUS
            | methods::APPLY_UPDATE
            // Execution configurations validate against the owner's disk.
            | methods::VALIDATE_EXECUTION_CONFIGURATION
            | methods::DESCRIBE_EXECUTION_REPOSITORY
            // A work-item launch validates and creates on the owner's disk.
            | methods::CREATE_CHAT_FOR_WORK_ITEM
            // A PR review start checks out and creates on the Space's owner.
            | methods::START_PR_REVIEW
            | methods::START_BRANCH_REVIEW
            // Planning artifacts: content lives on the owning device.
            | methods::ARTIFACT_CONTENT
            | methods::ARTIFACT_REGISTER
            | methods::ARTIFACT_PROMOTE
            | methods::ARTIFACT_DRIFT
    )
}

/// Forwardable methods whose reply is a stream (proxied item-by-item).
pub(super) fn is_stream_method(method: &str) -> bool {
    matches!(
        method,
        methods::WATCH_DOC_MESSAGES
            | methods::WATCH_QUEUE
            | methods::SUBSCRIBE_TERMINAL
            | methods::WATCH_CHECKOUT_DIFFS
            | methods::WATCH_WORKSPACE_GIT_STATUS
            | methods::WATCH_CHECKOUT_CHANGE_REQUEST
            | methods::WATCH_WORKSPACE_FILES
            | methods::WATCH_WORKSPACE_SEARCH_FILES
            | methods::WATCH_WORKSPACE_SYMBOLS
            | methods::SEARCH_WORKSPACE_CONTENT
            | methods::UPDATE_STATUS
    )
}

