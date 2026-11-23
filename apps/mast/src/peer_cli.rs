//! `mast peer`: pair this engine with another one over the direct link.
//!
//! On the machine that will do the work: `mast peer pair` prints a one-time
//! code, `mast peer list` and `mast peer revoke` manage who is paired. On the
//! controlling machine: `mast peer add <url> <code>` trades the code for a
//! token and saves it, and `mast peer config <name>` prints the MCP server
//! entry that points an agent at that peer.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mast_engine::peer_link::{PAIRING_PREFIX, PEER_PAIR_METHOD, PeerRegistry};
use serde::{Deserialize, Serialize};

const LINKS_FILE: &str = "peer-links.json";

#[derive(clap::Subcommand)]
pub enum PeerCommand {
    /// Open a five-minute pairing window and print the one-time code (run on the machine being driven).
    Pair,
    /// List paired peers.
    List,
    /// Revoke a paired peer by id, label or id prefix, or all of them.
    Revoke {
        key: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Pair with a peer's listener, e.g. `mast peer add fedora ws://100.101.102.103:27700 <code>`.
    Add { name: String, url: String, code: String },
    /// Forget a peer saved by `add` (does not revoke it on the peer; run `revoke` there too).
    Remove { name: String },
    /// Print the environment an MCP server needs to drive a saved peer.
    Config { name: String },
}

#[derive(Default, Serialize, Deserialize)]
struct Links {
    #[serde(default)]
    peers: Vec<SavedPeer>,
}

#[derive(Serialize, Deserialize)]
struct SavedPeer {
    name: String,
    url: String,
    token: String,
}

fn links_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LINKS_FILE)
}

fn load_links(data_dir: &Path) -> Result<Links> {
    match std::fs::read(links_path(data_dir)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).context("peer-links.json is not valid")?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Links::default()),
        Err(err) => Err(err.into()),
    }
}

fn save_links(data_dir: &Path, links: &Links) -> Result<()> {
    let path = links_path(data_dir);
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(links)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temp, &path)?;
    Ok(())
}

pub async fn run(command: PeerCommand, data_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    match command {
        PeerCommand::Pair => {
            let registry = PeerRegistry::open(data_dir)?;
            let code = registry.open_pairing();
            println!("Pairing code (valid 5 minutes, 5 attempts): {code}");
            println!("The engine must be running with MAST_PEER_LISTEN set to a loopback or tailnet address.");
        }
        PeerCommand::List => {
            let registry = PeerRegistry::open(data_dir)?;
            let peers = registry.list();
            if peers.is_empty() {
                println!("No paired peers.");
            }
            for peer in peers {
                println!("{}  {}  created {}  last used {}", peer.id, peer.label, peer.created_at_ms, peer.last_used_ms);
            }
        }
        PeerCommand::Revoke { key, all } => {
            let registry = PeerRegistry::open(data_dir)?;
            let count = match (key, all) {
                (_, true) => registry.revoke_all()?,
                (Some(key), false) => registry.revoke(&key).map_err(anyhow::Error::msg)?,
                (None, false) => bail!("name a peer or pass --all"),
            };
            println!("Revoked {count} peer(s). A running engine closes their connections within a few seconds.");
        }
        PeerCommand::Add { name, url, code } => {
            let client = mast_rpc::connect_ws_bearer(&url, Some(&format!("{PAIRING_PREFIX}{code}")))
                .await
                .with_context(|| format!("could not reach {url}"))?;
            let label = mast_proto::env::var("MAST_DEVICE_NAME").unwrap_or_else(|_| name.clone());
            let reply = client
                .call(PEER_PAIR_METHOD, serde_json::json!({ "label": label }))
                .await
                .context("pairing was refused")?;
            let token = reply
                .get("token")
                .and_then(|v| v.as_str())
                .context("the peer did not return a token")?
                .to_string();
            let mut links = load_links(data_dir)?;
            links.peers.retain(|p| p.name != name);
            links.peers.push(SavedPeer { name: name.clone(), url, token });
            save_links(data_dir, &links)?;
            println!("Paired with {name}. Run `mast peer config {name}` to wire it into an agent.");
        }
        PeerCommand::Remove { name } => {
            let mut links = load_links(data_dir)?;
            let before = links.peers.len();
            links.peers.retain(|p| p.name != name);
            save_links(data_dir, &links)?;
            println!("Removed {} saved peer(s).", before - links.peers.len());
        }
        PeerCommand::Config { name } => {
            let links = load_links(data_dir)?;
            let peer = links.peers.iter().find(|p| p.name == name).with_context(|| format!("no saved peer named {name}"))?;
            println!("# MCP server entry for the peer '{name}': a second `mast mcp` that proxies that engine.");
            println!("MAST_PEER_URL={}", peer.url);
            println!("MAST_PEER_TOKEN={}", peer.token);
            println!("# command: mast mcp   (set the two variables above in the server's env; the token is a secret)");
        }
    }
    Ok(())
}
