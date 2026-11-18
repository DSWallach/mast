//! Named agent definitions (the `agent` model option) on harnesses without a
//! native `--agent` flag.
//!
//! Claude Code resolves `--agent <name>` itself. Codex and Antigravity have no
//! such flag, so Mast finds the definition file and delivers its instructions
//! through the harness's own instruction channel (Codex
//! `developerInstructions`, Antigravity's system-prompt append), ahead of any
//! Work Outline envelope. Only the instructions are used there: a definition's
//! `model` or `tools` fields are ignored, so the run's picked model wins.
//!
//! Lookup is project before user, and within a level the harness's own format
//! first. The project is the git checkout containing the run's cwd.

use std::path::{Path, PathBuf};

use mast_proto::{HarnessId, RunRequest};

use crate::{HarnessCapabilities, HarnessError, InstructionSupport};

/// Largest definition file Mast reads.
const MAX_DEFINITION_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// Codex: `name`, `description`, `developer_instructions` at top level.
    CodexToml,
    /// Claude Code: optional YAML frontmatter, then the system prompt.
    ClaudeMarkdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentDefinition {
    pub name: String,
    pub path: PathBuf,
    pub instructions: String,
}

fn agent_error(harness: HarnessId, agent: &str, detail: impl Into<String>) -> HarnessError {
    HarnessError::Agent {
        harness,
        agent: agent.to_owned(),
        detail: detail.into(),
    }
}

/// Refuse a run that names an agent on a harness without named agents,
/// rather than run it as a plain chat.
pub fn require_named_agent_support(
    harness: HarnessId,
    capabilities: HarnessCapabilities,
    request: &RunRequest,
) -> Result<(), HarnessError> {
    match mast_proto::agent_option(&request.model_options) {
        Some(agent) if !capabilities.named_agents => Err(agent_error(
            harness,
            agent,
            "this harness cannot run a named agent; use Claude Code, Codex or Antigravity",
        )),
        _ => Ok(()),
    }
}

/// The run's agent name when it is valid; an error when it is set but not a
/// plain name.
pub(crate) fn request_agent(
    harness: HarnessId,
    request: &RunRequest,
) -> Result<Option<String>, HarnessError> {
    match mast_proto::valid_agent_option(&request.model_options) {
        Ok(name) => Ok(name.map(str::to_owned)),
        Err(detail) => Err(agent_error(
            harness,
            mast_proto::agent_option(&request.model_options).unwrap_or_default(),
            detail,
        )),
    }
}

/// The Codex user directory: the managed account's `CODEX_HOME`, else the
/// process's `CODEX_HOME`, else `<home>/.codex`.
pub(crate) fn codex_user_dir(account_home: Option<&Path>) -> Option<PathBuf> {
    account_home
        .map(Path::to_path_buf)
        .or_else(|| {
            std::env::var_os("CODEX_HOME")
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| crate::executable::home_dir().map(|home| home.join(".codex")))
}

/// Resolve the request's agent definition off the async runtime. `None`
/// when the request names no agent.
pub(crate) async fn resolve_for_request(
    harness: HarnessId,
    request: &RunRequest,
    codex_dir: Option<PathBuf>,
) -> Result<Option<AgentDefinition>, HarnessError> {
    let Some(name) = request_agent(harness, request)? else {
        return Ok(None);
    };
    let cwd = (!request.cwd.is_empty()).then(|| PathBuf::from(&request.cwd));
    let home = crate::executable::home_dir();
    tokio::task::spawn_blocking(move || {
        let repo = cwd.as_deref().map(repo_root);
        resolve(
            harness,
            &name,
            repo.as_deref(),
            home.as_deref(),
            codex_dir.as_deref(),
        )
    })
    .await
    .map_err(|error| HarnessError::Protocol(format!("agent definition lookup failed: {error}")))?
    .map(Some)
}

/// The git checkout containing `cwd` (the nearest ancestor with a `.git`
/// entry, which is a file in a linked worktree), else `cwd` itself.
fn repo_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// Where `harness` looks for `name`, in order.
fn candidates(
    harness: HarnessId,
    name: &str,
    repo: Option<&Path>,
    home: Option<&Path>,
    codex_dir: Option<&Path>,
) -> Vec<(PathBuf, Format)> {
    let claude = |root: &Path| {
        (
            root.join(".claude").join("agents").join(format!("{name}.md")),
            Format::ClaudeMarkdown,
        )
    };
    let codex = |dir: &Path| {
        (
            dir.join("agents").join(format!("{name}.toml")),
            Format::CodexToml,
        )
    };
    let mut out = Vec::new();
    if harness == HarnessId::Codex {
        out.extend(repo.map(|repo| codex(&repo.join(".codex"))));
        out.extend(repo.map(claude));
        out.extend(codex_dir.map(codex));
        out.extend(home.map(claude));
    } else {
        out.extend(repo.map(claude));
        out.extend(home.map(claude));
    }
    out
}

fn resolve(
    harness: HarnessId,
    name: &str,
    repo: Option<&Path>,
    home: Option<&Path>,
    codex_dir: Option<&Path>,
) -> Result<AgentDefinition, HarnessError> {
    let candidates = candidates(harness, name, repo, home, codex_dir);
    for (path, format) in &candidates {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(agent_error(
                    harness,
                    name,
                    format!("could not read {}: {error}", path.display()),
                ));
            }
        };
        if metadata.len() > MAX_DEFINITION_BYTES {
            return Err(agent_error(
                harness,
                name,
                format!(
                    "{} is larger than {MAX_DEFINITION_BYTES} bytes",
                    path.display()
                ),
            ));
        }
        let text = std::fs::read_to_string(path).map_err(|error| {
            agent_error(
                harness,
                name,
                format!("could not read {}: {error}", path.display()),
            )
        })?;
        let instructions = match format {
            Format::CodexToml => codex_instructions(&text),
            Format::ClaudeMarkdown => markdown_body(&text),
        }
        .map_err(|detail| agent_error(harness, name, format!("{}: {detail}", path.display())))?;
        return Ok(AgentDefinition {
            name: name.to_owned(),
            path: path.clone(),
            instructions,
        });
    }
    let searched = candidates
        .iter()
        .map(|(path, _)| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(agent_error(
        harness,
        name,
        format!("no agent definition found; looked in {searched}"),
    ))
}

fn codex_instructions(text: &str) -> Result<String, String> {
    let table: toml::Table =
        toml::from_str(text).map_err(|error| format!("not valid TOML: {error}"))?;
    let instructions = match table.get("developer_instructions") {
        Some(toml::Value::String(text)) => text.trim(),
        Some(_) => return Err("`developer_instructions` is not a string".into()),
        None => return Err("no `developer_instructions`".into()),
    };
    if instructions.is_empty() {
        return Err("`developer_instructions` is empty".into());
    }
    Ok(instructions.to_owned())
}

/// The body of a Claude Code agent file: everything after the YAML
/// frontmatter (or the whole file when it has none).
fn markdown_body(text: &str) -> Result<String, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    let body = match lines.next() {
        Some(first) if first.trim_end() == "---" => {
            let mut offset = first.len();
            let mut closed = false;
            for line in lines {
                offset += line.len();
                if line.trim_end() == "---" {
                    closed = true;
                    break;
                }
            }
            if !closed {
                return Err("the frontmatter has no closing `---`".into());
            }
            &text[offset..]
        }
        _ => text,
    };
    let body = body.trim();
    if body.is_empty() {
        return Err("the definition has no instructions after its frontmatter".into());
    }
    Ok(body.to_owned())
}

/// The text a developer-instructions or system-prompt channel carries: the
/// agent's instructions first, then the Work Outline envelope when present.
pub(crate) fn compose(agent: &AgentDefinition, envelope: Option<&str>) -> String {
    let mut text = agent.instructions.trim_end().to_owned();
    text.push('\n');
    if let Some(envelope) = envelope.filter(|envelope| !envelope.is_empty()) {
        text.push('\n');
        text.push_str(envelope);
    }
    text
}

/// Refuse a composed text the harness's instruction channel cannot carry.
pub(crate) fn require_channel_fits(
    harness: HarnessId,
    agent: &AgentDefinition,
    support: &InstructionSupport,
    composed: &str,
) -> Result<(), HarnessError> {
    match support {
        InstructionSupport::Unsupported {
            reason,
            upgrade_hint,
        } => {
            let hint = upgrade_hint
                .as_deref()
                .map(|hint| format!(" ({hint})"))
                .unwrap_or_default();
            Err(agent_error(
                harness,
                &agent.name,
                format!("{reason}{hint}"),
            ))
        }
        InstructionSupport::Supported { max_bytes, .. } if composed.len() > *max_bytes => {
            Err(agent_error(
                harness,
                &agent.name,
                format!(
                    "its instructions{} are {} bytes; this harness accepts at most {max_bytes}",
                    if composed.len() > agent.instructions.len() + 1 {
                        " with the Work Outline instructions"
                    } else {
                        ""
                    },
                    composed.len()
                ),
            ))
        }
        InstructionSupport::Supported { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dirs {
        _root: tempfile::TempDir,
        repo: PathBuf,
        home: PathBuf,
        codex: PathBuf,
    }

    fn dirs() -> Dirs {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let home = root.path().join("home");
        let codex = home.join("codex-home");
        for dir in [&repo, &home, &codex] {
            std::fs::create_dir_all(dir).unwrap();
        }
        Dirs {
            _root: root,
            repo,
            home,
            codex,
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn claude_md(root: &Path) -> PathBuf {
        root.join(".claude").join("agents").join("pr-review.md")
    }

    fn codex_toml(dir: &Path) -> PathBuf {
        dir.join("agents").join("pr-review.toml")
    }

    fn found(harness: HarnessId, d: &Dirs) -> Result<AgentDefinition, HarnessError> {
        resolve(
            harness,
            "pr-review",
            Some(&d.repo),
            Some(&d.home),
            Some(&d.codex),
        )
    }

    const MD: &str = "---\nname: pr-review\nmodel: opus\ntools: Read\n---\n\nReview as {who}.\n";

    fn md(who: &str) -> String {
        MD.replace("{who}", who)
    }

    fn toml_def(who: &str) -> String {
        format!(
            "name = \"pr-review\"\ndescription = \"d\"\nmodel = \"x\"\ndeveloper_instructions = \"\"\"\nReview as {who}.\n\"\"\"\n"
        )
    }

    #[test]
    fn codex_looks_in_project_then_user_and_its_own_format_first() {
        let d = dirs();
        write(&claude_md(&d.home), &md("user claude"));
        assert_eq!(
            found(HarnessId::Codex, &d).unwrap().instructions,
            "Review as user claude."
        );
        write(&codex_toml(&d.codex), &toml_def("user codex"));
        assert_eq!(
            found(HarnessId::Codex, &d).unwrap().instructions,
            "Review as user codex."
        );
        write(&claude_md(&d.repo), &md("project claude"));
        assert_eq!(
            found(HarnessId::Codex, &d).unwrap().instructions,
            "Review as project claude."
        );
        write(&codex_toml(&d.repo.join(".codex")), &toml_def("project codex"));
        let definition = found(HarnessId::Codex, &d).unwrap();
        assert_eq!(definition.instructions, "Review as project codex.");
        assert_eq!(definition.path, codex_toml(&d.repo.join(".codex")));
    }

    #[test]
    fn antigravity_reads_only_claude_definitions_project_first() {
        let d = dirs();
        write(&codex_toml(&d.repo.join(".codex")), &toml_def("codex"));
        write(&claude_md(&d.home), &md("user"));
        assert_eq!(
            found(HarnessId::Antigravity, &d).unwrap().instructions,
            "Review as user."
        );
        write(&claude_md(&d.repo), &md("project"));
        assert_eq!(
            found(HarnessId::Antigravity, &d).unwrap().instructions,
            "Review as project."
        );
    }

    #[test]
    fn a_missing_definition_names_every_path_searched() {
        let d = dirs();
        let error = found(HarnessId::Codex, &d).unwrap_err().to_string();
        assert!(error.contains("\"pr-review\""), "{error}");
        for path in [
            codex_toml(&d.repo.join(".codex")),
            claude_md(&d.repo),
            codex_toml(&d.codex),
            claude_md(&d.home),
        ] {
            assert!(error.contains(&path.display().to_string()), "{error}");
        }
    }

    #[test]
    fn invalid_definitions_fail_instead_of_falling_through() {
        let d = dirs();
        write(&claude_md(&d.home), &md("user"));
        for bad in [
            "---\nname: x\n",
            "---\nname: x\n---\n   \n",
        ] {
            write(&claude_md(&d.repo), bad);
            assert!(
                matches!(found(HarnessId::Antigravity, &d), Err(HarnessError::Agent { .. })),
                "{bad:?}"
            );
        }
        std::fs::remove_file(claude_md(&d.repo)).unwrap();
        for bad in [
            "name = \"x\"\n",
            "developer_instructions = 3\n",
            "developer_instructions = \"  \"\n",
            "not toml = = =",
        ] {
            write(&codex_toml(&d.codex), bad);
            let error = found(HarnessId::Codex, &d).unwrap_err().to_string();
            assert!(error.contains(&codex_toml(&d.codex).display().to_string()), "{error}");
        }
    }

    #[test]
    fn markdown_without_frontmatter_is_all_instructions() {
        assert_eq!(markdown_body("Just do it.\n").unwrap(), "Just do it.");
        assert_eq!(
            markdown_body("\u{feff}---\r\na: b\r\n---\r\nBody\r\n").unwrap(),
            "Body"
        );
    }

    #[test]
    fn the_repo_root_is_the_checkout_containing_the_cwd() {
        let d = dirs();
        let nested = d.repo.join("crates").join("x");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(repo_root(&nested), nested);
        std::fs::write(d.repo.join(".git"), "gitdir: elsewhere\n").unwrap();
        assert_eq!(repo_root(&nested), d.repo);
    }

    #[test]
    fn composition_puts_the_agent_first_and_respects_the_channel_size() {
        let agent = AgentDefinition {
            name: "pr-review".into(),
            path: PathBuf::from("x"),
            instructions: "Review.".into(),
        };
        assert_eq!(compose(&agent, None), "Review.\n");
        assert_eq!(
            compose(&agent, Some("<envelope/>\n")),
            "Review.\n\n<envelope/>\n"
        );
        let support = |max_bytes| InstructionSupport::Supported {
            channel: crate::InstructionChannel::DeveloperInstructions,
            replace_strategy: crate::InstructionReplaceStrategy::FreshSession,
            max_bytes,
        };
        assert!(require_channel_fits(HarnessId::Codex, &agent, &support(8), "Review.\n").is_ok());
        let error =
            require_channel_fits(HarnessId::Codex, &agent, &support(7), "Review.\n").unwrap_err();
        assert!(error.to_string().contains("at most 7"), "{error}");
        let error = require_channel_fits(
            HarnessId::Codex,
            &agent,
            &InstructionSupport::Unsupported {
                reason: "too old".into(),
                upgrade_hint: Some("Update Codex".into()),
            },
            "Review.\n",
        )
        .unwrap_err();
        assert!(error.to_string().contains("too old (Update Codex)"), "{error}");
    }

    #[test]
    fn only_harnesses_with_named_agents_accept_the_option() {
        let mut request = RunRequest {
            prompt: "p".into(),
            harness: None,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: String::new(),
            sandbox: mast_proto::SandboxLevel::WorkspaceWrite,
            auto_approve: false,
            approval_rules: false,
            scheduled_task: None,
            resume: None,
            fork_from: None,
            attachments: Vec::new(),
            worktree: None,
            instructions: None,
        };
        let cursor = crate::capabilities(HarnessId::Cursor);
        assert!(require_named_agent_support(HarnessId::Cursor, cursor, &request).is_ok());
        request
            .model_options
            .insert(mast_proto::AGENT_OPTION.into(), "pr-review".into());
        assert!(matches!(
            require_named_agent_support(HarnessId::Cursor, cursor, &request),
            Err(HarnessError::Agent { .. })
        ));
        for id in [
            HarnessId::ClaudeCode,
            HarnessId::Codex,
            HarnessId::Antigravity,
            HarnessId::Opencode,
        ] {
            assert!(require_named_agent_support(id, crate::capabilities(id), &request).is_ok());
        }
        request
            .model_options
            .insert(mast_proto::AGENT_OPTION.into(), "../x".into());
        assert!(request_agent(HarnessId::Codex, &request).is_err());
    }
}
