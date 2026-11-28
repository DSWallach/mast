//! Wire shapes of the methods only a paired peer calls (see the engine's
//! `peer_link`): branch transfer over the link and worker-report delivery.
//!
//! None of these are engine RPC methods. The peer listener's gate answers them
//! itself, for the authenticated peer only; the main engine service never sees
//! them.
use serde::{Deserialize, Serialize};

/// Append one chunk of a bundle the peer is sending.
pub const PEER_GIT_PUT: &str = "PeerGitPut";
/// Verify a fully received bundle and turn it into a peer-owned branch and worktree.
pub const PEER_GIT_APPLY: &str = "PeerGitApply";
/// Bundle the commits a peer-owned branch gained since a base commit, for the peer to fetch.
pub const PEER_GIT_BUNDLE: &str = "PeerGitBundle";
/// Read one range of a bundle made by [`PEER_GIT_BUNDLE`].
pub const PEER_GIT_READ: &str = "PeerGitRead";
/// Drain the reports this peer's workers queued for a parent chat on the peer.
pub const PEER_REPORTS: &str = "PeerReports";
/// Acknowledge delivered reports, so the sender forgets them.
pub const PEER_REPORTS_ACK: &str = "PeerReportsAck";
/// Read the result of a command hosted by a runner this peer created.
pub const PEER_COMMAND_STATUS: &str = "PeerCommandStatus";
/// Start a Lead on this device. Only the paired Manager's own engine calls
/// it, after it admitted the Lead against the Manager's whole project; a bare
/// `Mutate createChat` that names a Lead is refused.
pub const PEER_LEAD_CREATE: &str = "PeerLeadCreate";
/// The Leads this device holds for the calling peer that still count against
/// the project's Lead limit.
pub const PEER_LEADS: &str = "PeerLeads";

/// What [`PEER_LEAD_CREATE`] carries: a `createChat` plus the Lead's identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerLeadCreateParams {
    pub chat_id: String,
    pub device_id: String,
    pub space_id: String,
    /// The Manager's chat on the Manager's device.
    pub parent_chat_id: String,
    /// The Lead's work item in the Manager's registry (opaque here).
    pub lead_item_id: String,
    #[serde(default)]
    pub lead_owned_paths: Vec<String>,
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerLeadInfo {
    pub chat_id: String,
    pub lead_item_id: String,
    pub parent_chat_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerLeadsReply {
    pub leads: Vec<PeerLeadInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerCommandStatusParams {
    pub chat_id: String,
    pub parent_chat_id: String,
    pub idempotency_key: String,
}

/// Raw bytes per transfer chunk (the wire carries them base64-encoded).
pub const PEER_GIT_CHUNK_BYTES: usize = 384 * 1024;
/// Largest bundle either side accepts.
pub const PEER_GIT_MAX_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;
/// Most reports one `PeerReports` call returns.
pub const PEER_REPORTS_PAGE: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerGitPutParams {
    /// Chosen by the sender; one id per bundle.
    pub transfer_id: String,
    /// Where this chunk starts. A chunk the receiver already has is accepted
    /// again without effect, so a lost reply is safe to retry.
    pub offset: u64,
    /// Base64 of the chunk bytes.
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerGitPutReply {
    /// Bytes held for this transfer so far.
    pub received: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerGitApplyParams {
    pub transfer_id: String,
    /// A Space on the receiving device; its repository gets the branch.
    pub space_id: String,
    /// Branch name as the sender knows it (`feature/x`); the receiver files it
    /// under the peer's own namespace.
    pub branch: String,
    /// Total bundle size, so a short upload is refused.
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerGitApplyReply {
    pub space_id: String,
    /// The branch on the receiving device (`peer/<peer>/<branch>`), checked out in the worktree.
    pub peer_branch: String,
    /// The commit that branch is at.
    pub head: String,
    /// Absolute path of the worktree on the receiving device; start the worker there.
    pub worktree_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerGitBundleParams {
    pub space_id: String,
    /// The branch name used when sending it (not the namespaced one).
    pub branch: String,
    /// A full commit id the sender already has (the `head` it pushed); only
    /// commits after it are bundled. Omitted means the whole history.
    #[serde(default)]
    pub base: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerGitBundleReply {
    pub transfer_id: String,
    pub size: u64,
    /// The commit the bundled branch is at, and its ref name inside the bundle.
    pub head: String,
    pub ref_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerGitReadParams {
    pub transfer_id: String,
    pub offset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerGitReadReply {
    /// Base64 of up to [`PEER_GIT_CHUNK_BYTES`] bytes from `offset`.
    pub data: String,
    pub eof: bool,
}

/// One worker report waiting to be delivered to its parent chat on the peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerReportItem {
    /// Stable for the life of the report: the receiver's duplicate guard.
    pub id: String,
    /// The reporting worker's chat on the sending device.
    pub chat_id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// The parent chat on the receiving device, as named when the worker was created.
    pub parent_chat_id: String,
    pub text: String,
    pub created_at_ms: i64,
    /// Set when the reporting chat is a Lead (`leadItemId` it was created
    /// with): `text` is then already a bounded Lead digest, and the receiver
    /// bounds it again if a sender left it whole. Absent from a sender that
    /// predates the Lead tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_item_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerReportsParams {
    /// Reports to pass over (oldest first) before the page starts. The collector
    /// sets it to the number it could not deliver yet, so reports for a parent
    /// that cannot take them (an archived chat) do not hold back the rest. A
    /// sender that does not know this field returns the first page, which the
    /// collector recognizes as nothing new.
    #[serde(default)]
    pub skip: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerReportsReply {
    pub reports: Vec<PeerReportItem>,
    /// Parent chats on the collector's device that this peer's workers name.
    /// The collector remembers them, so that when the peer later goes
    /// unreachable it knows whose reports may be waiting.
    #[serde(default)]
    pub parents: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerReportsAckParams {
    pub ids: Vec<String>,
}
