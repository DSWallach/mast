//! Synthetic input for native upgrade tests. The profile and SQLite layout are
//! from v0.2.82. Outline, artifact, scheduled pin, and peer records use their
//! native encoders so later slices can exercise preserving those records too.
//! Include from an engine integration test with `#[path = ".../upgrade_fixture.rs"]`.

use std::fs;
use std::path::{Path, PathBuf};

use mast_doc::{
    MessagePart, MessageRole, REGISTRY_DOC_ID, RegistryDoc, SessionDoc, SessionMessageEntry,
};
use mast_proto::work_outline::{
    CreateWorkItemParams, WorkOutlineActor, WorkOutlineActorKind, WorkOutlineMutation,
};
use mast_proto::{
    ArtifactComment, ArtifactKind, ArtifactRevision, ArtifactView, CommentAnchor, ScheduledChat,
    SidebarPinChange, SidebarSectionChange, Space,
};
use rusqlite::{Connection, params};
use serde_json::json;
use sha2::{Digest, Sha256};

pub const DEVICE_ID: &str = "upgrade-fixture-device";
pub const SPACE_ID: &str = "upgrade-fixture-space";
pub const CHAT_ID: &str = "upgrade-fixture-chat";
pub const CHAT_TITLE: &str = "Fixture chat with a pending title";
pub const MESSAGE_ID: &str = "upgrade-fixture-message";
pub const UPLOAD_NAME: &str = "f17e0001-pixel.png";
pub const OUTLINE_OPERATION_ID: &str = "upgrade-fixture-create-item";
pub const TASK_ID: &str = "upgrade-fixture-task";
pub const ARTIFACT_ID: &str = "upgrade-fixture-artifact";
pub const COMMENT_ID: &str = "C-1";
pub const PEER_NAME: &str = "Fixture Mast peer";

const AT: i64 = 1_700_000_000_000;
const PROFILE_ID: &str = "00000000-0000-4000-8000-000000000282";
const ARTIFACT_HTML: &str = "<p id=\"summary\">Synthetic upgrade artifact</p>\n";

// v0.2.82's four DocsStore migrations, folded into the final schema. Keep
// this independent of DocsStore::open so the upgrade opens release-era input.
const DOCS_SCHEMA: &str = "
    CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL) STRICT;
    CREATE TABLE snapshots (
        doc_id TEXT PRIMARY KEY, bytes BLOB NOT NULL, saved_at INTEGER NOT NULL,
        cursor INTEGER, epoch INTEGER, cursor_verified INTEGER NOT NULL DEFAULT 0
    ) STRICT;
    CREATE TABLE processed_commands (command_id TEXT PRIMARY KEY, processed_at INTEGER NOT NULL) STRICT;
    CREATE TABLE chat_outbox (
        ordinal INTEGER PRIMARY KEY AUTOINCREMENT, doc_id TEXT NOT NULL,
        batch_id TEXT NOT NULL UNIQUE, bytes BLOB NOT NULL,
        needs_checkpoint INTEGER NOT NULL DEFAULT 0
    ) STRICT;
    CREATE INDEX chat_outbox_doc ON chat_outbox(doc_id, ordinal);
    CREATE TABLE chat_outbox_initialized (doc_id TEXT PRIMARY KEY) STRICT;
";

#[derive(Clone, Copy, Debug)]
pub enum ProfileLayout {
    NativeMast,
    LegacyZeron,
}

pub struct UpgradeFixture {
    // Own the scratch directory so absolute paths in the persisted records stay
    // valid for the lifetime of the fixture.
    _temp: tempfile::TempDir,
    pub home: PathBuf,
    pub data_dir: PathBuf,
    pub profile_dir: PathBuf,
    pub upload_path: PathBuf,
    pub artifact_path: PathBuf,
    pub work_item_id: String,
}

impl UpgradeFixture {
    /// Seed one layout in a fresh, isolated home. Child processes can use
    /// `home` as HOME to exercise default path selection without an override.
    pub fn new(layout: ProfileLayout) -> Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::Builder::new().prefix("mast-upgrade-").tempdir()?;
        let home = temp.path().canonicalize()?;
        let data_dir = home.join(match layout {
            ProfileLayout::NativeMast => ".mast",
            ProfileLayout::LegacyZeron => ".zeron",
        });
        let profile_dir = data_dir.join("profiles/local");
        let project = home.join("project");
        let upload_path = profile_dir.join("uploads").join(UPLOAD_NAME);
        let artifact_folder = "_chats/upgrade-fixture-chat";
        let artifact_path = project
            .join(match layout {
                ProfileLayout::NativeMast => ".mast/artifacts",
                ProfileLayout::LegacyZeron => ".zeron/artifacts",
            })
            .join(artifact_folder)
            .join("summary.html");

        write(
            &data_dir.join("local-profile.json"),
            json!({"id": PROFILE_ID}).to_string(),
        )?;
        write(&data_dir.join("device-id"), DEVICE_ID)?;
        write(
            &data_dir.join("harness-prefs.json"),
            r#"{"disabled":["codex"]}"#,
        )?;
        write(
            &data_dir.join("peer-links.json"),
            json!({"peers": [{
                "name": PEER_NAME,
                "url": "http://127.0.0.1:1",
                "token": "synthetic-fixture-token"
            }]})
            .to_string(),
        )?;
        // A synthetic 1x1 PNG, also referenced by the transcript.
        write(
            &upload_path,
            [
                137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0,
                1, 8, 4, 0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100,
                248, 15, 0, 1, 5, 1, 1, 39, 24, 227, 102, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96,
                130,
            ],
        )?;
        write(&artifact_path, ARTIFACT_HTML)?;
        let hash = format!("{:x}", Sha256::digest(ARTIFACT_HTML));
        write(
            &profile_dir.join("artifacts").join(ARTIFACT_ID).join(&hash),
            ARTIFACT_HTML,
        )?;

        let at = chrono::DateTime::from_timestamp_millis(AT).expect("valid fixture timestamp");
        let mut registry = RegistryDoc::new(DEVICE_ID);
        registry.upsert_space(&Space {
            id: SPACE_ID.into(),
            device_id: DEVICE_ID.into(),
            path: project.to_string_lossy().into_owned(),
            name: Some("Upgrade fixture".into()),
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: at,
        })?;
        registry.claim_chat(CHAT_ID, project.to_str(), Some(SPACE_ID), at);
        registry.rename_chat(CHAT_ID, "Fixture chat")?;
        registry.set_chat_scheduled_task_info(
            CHAT_ID,
            &ScheduledChat {
                task_id: TASK_ID.into(),
                task_name: "Fixture scheduled task".into(),
                run_id: "upgrade-fixture-run".into(),
                session_created_at: Some(AT),
                ..ScheduledChat::default()
            },
        )?;
        registry.change_sidebar_pin(&SidebarPinChange::Section {
            change: SidebarSectionChange::PinScheduled {
                task_id: TASK_ID.into(),
                pinned: true,
            },
        })?;

        let actor = WorkOutlineActor {
            kind: WorkOutlineActorKind::User,
            device_id: DEVICE_ID.into(),
            ..WorkOutlineActor::default()
        };
        registry.apply_work_outline_mutation(
            &WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
                space_id: SPACE_ID.into(),
                parent_work_item_id: None,
                placement: Default::default(),
                title: "Verify upgraded profile".into(),
                description: None,
                status: None,
                operation_id: OUTLINE_OPERATION_ID.into(),
            }),
            &actor,
            AT,
        )?;
        let work_item_id = mast_proto::derive_id(OUTLINE_OPERATION_ID, "item");
        registry.put_artifact(&ArtifactView {
            id: ARTIFACT_ID.into(),
            space_id: SPACE_ID.into(),
            chat_id: Some(CHAT_ID.into()),
            device_id: DEVICE_ID.into(),
            folder: artifact_folder.into(),
            stored_path: artifact_path.to_string_lossy().into_owned(),
            title: "Upgrade artifact".into(),
            kind: ArtifactKind::HtmlFragment,
            current_revision: 1,
            revisions: vec![ArtifactRevision {
                id: 1,
                hash,
                at: AT,
                by: actor.clone(),
                size_bytes: ARTIFACT_HTML.len() as u64,
                ..ArtifactRevision::default()
            }],
            comments: vec![ArtifactComment {
                id: COMMENT_ID.into(),
                anchor: CommentAnchor {
                    id: Some("summary".into()),
                    ..CommentAnchor::default()
                },
                text: "Keep this comment during upgrade".into(),
                revision: 1,
                at: AT,
                by: actor,
                ..ArtifactComment::default()
            }],
            created_at: AT,
            updated_at: AT,
            ..ArtifactView::default()
        })?;
        registry.settle_pending_locally();
        // Write exactly one unacknowledged registry op over the settled rows.
        registry.rename_chat(CHAT_ID, CHAT_TITLE)?;

        let transcript = SessionDoc::init(CHAT_ID)?;
        transcript.push_message(&SessionMessageEntry {
            id: MESSAGE_ID.into(),
            role: MessageRole::User,
            parts: vec![
                MessagePart::Text {
                    id: "text".into(),
                    text: "Hello from the upgrade fixture".into(),
                },
                MessagePart::Image {
                    id: "image".into(),
                    path: upload_path.to_string_lossy().into_owned(),
                    name: "pixel.png".into(),
                    mime_type: "image/png".into(),
                },
            ],
            created_at: AT,
            device_id: DEVICE_ID.into(),
            status: None,
            continuation_of: None,
        })?;

        let db = Connection::open(profile_dir.join("docs.sqlite3"))?;
        db.execute_batch(DOCS_SCHEMA)?;
        db.execute(
            "INSERT INTO schema_migrations VALUES (1, ?1), (2, ?1), (3, ?1), (4, ?1)",
            [AT],
        )?;
        for (id, bytes) in [
            (REGISTRY_DOC_ID, registry.to_bytes()?),
            (CHAT_ID, transcript.export_snapshot()?),
        ] {
            db.execute(
                "INSERT INTO snapshots (doc_id, bytes, saved_at) VALUES (?1, ?2, ?3)",
                params![id, bytes, AT],
            )?;
        }

        Ok(Self {
            _temp: temp,
            home,
            data_dir,
            profile_dir,
            upload_path,
            artifact_path,
            work_item_id,
        })
    }
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    fs::create_dir_all(path.parent().expect("fixture file has a parent"))?;
    fs::write(path, bytes)
}
