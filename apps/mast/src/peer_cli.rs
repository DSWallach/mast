//! `mast peer`: pair this engine with another one over the direct link.
//!
//! On the machine that will do the work: `mast peer pair` prints a one-time
//! code, `mast peer list` and `mast peer revoke` manage who is paired. On the
//! controlling machine: `mast peer add <url> <code>` trades the code for a
//! token and saves it, and `mast peer config <name>` prints the MCP server
//! entry that points an agent at that peer.
use std::path::Path;

use anyhow::{Context, Result, bail};
use mast_engine::peer_link::{PAIRING_PREFIX, PEER_PAIR_METHOD, PeerRegistry, PeerScope};
use mast_engine::peer_outbound::{Links, SavedPeer};

#[derive(clap::Subcommand)]
pub enum PeerCommand {
    /// Open a five-minute pairing window and print the one-time code (run on the machine being driven).
    Pair,
    /// List paired peers.
    List,
    /// Choose what a paired peer may reach: `own` (only chats it created through the link) or
    /// `all` (every chat on this machine). Pairings made before scopes existed are `all`.
    Scope { key: String, scope: String },
    /// Revoke a paired peer by id, label or id prefix, or all of them.
    Revoke {
        key: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Pair with a peer's listener, e.g. `mast peer add fedora ws://100.101.102.103:27700 <code>`.
    /// The code is read from stdin when omitted, so it stays out of shell history.
    Add { name: String, url: String, code: Option<String> },
    /// Forget a peer saved by `add` (does not revoke it on the peer; run `revoke` there too).
    Remove { name: String },
    /// Print the environment an MCP server needs to drive a saved peer.
    Config { name: String },
}

fn load_links(data_dir: &Path) -> Result<Links> {
    Ok(mast_engine::peer_outbound::load_links(data_dir)?)
}

fn save_links(data_dir: &Path, links: &Links) -> Result<()> {
    Ok(mast_engine::peer_outbound::save_links(data_dir, links)?)
}

pub async fn run(command: PeerCommand, data_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    match command {
        PeerCommand::Pair => {
            let registry = PeerRegistry::open(data_dir)?;
            let code = registry.open_pairing()?;
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
                let (chats, reports) = registry.peer_counts(&peer.id);
                let scope = match peer.scope {
                    PeerScope::Own => "own chats".to_string(),
                    PeerScope::All => "all chats (set `mast peer scope <peer> own` to narrow)".to_string(),
                };
                println!(
                    "{}  {}  scope {scope}  chats created {chats}  reports waiting {reports}  created {}  last used {}",
                    peer.id, peer.label, peer.created_at_ms, peer.last_used_ms
                );
            }
            let links = load_links(data_dir)?;
            if !links.peers.is_empty() {
                let status = mast_engine::peer_outbound::load_status(data_dir);
                println!("Saved peers this device polls for worker reports:");
                for peer in &links.peers {
                    match status.get(&peer.name) {
                        Some(s) => println!("{}  {}  last poll: {}{}", peer.name, peer.url, s.state, s.detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default()),
                        None => println!("{}  {}  last poll: none recorded (is the engine running?)", peer.name, peer.url),
                    }
                }
            }
        }
        PeerCommand::Scope { key, scope } => {
            let scope = PeerScope::parse(&scope).with_context(|| "scope is `own` or `all`")?;
            let registry = PeerRegistry::open(data_dir)?;
            let count = registry.set_scope(&key, scope).map_err(anyhow::Error::msg)?;
            if count == 0 {
                bail!("no paired peer matches {key}");
            }
            println!("{key} now reaches {} (a running engine applies it within a second).", scope.as_str());
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
            let code = match code {
                Some(code) => code,
                None => {
                    eprintln!("Pairing code:");
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    line.trim().to_string()
                }
            };
            let client = mast_rpc::connect_ws_bearer(&url, Some(&format!("{PAIRING_PREFIX}{code}")))
                .await
                .with_context(|| format!("could not reach {url}, or pairing is not open (run `mast peer pair` on the peer first)"))?;
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
