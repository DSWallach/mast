use super::*;

/// the user-global skill folders the server loads (`resolve_skills_paths`).
/// its project-level `.gemini/skills` and `.agents/skills` depend on a session
/// cwd the command listing doesn't have, so those still reach the agent when
/// typed but aren't listed.
pub(super) fn antigravity_skill_dirs() -> Vec<PathBuf> {
    antigravity_paths::home()
        .map(|home| {
            vec![
                home.join("config").join("skills"),
                home.join("antigravity-cli").join("skills"),
                home.join("antigravity-cli").join("builtin").join("skills"),
            ]
        })
        .unwrap_or_default()
}

/// the pinned server keeps accepting `vertex-ai` for the method its
/// `initialize` advertises as `agent-platform`, so a settings file saved under
/// the old name still has to resolve.
pub(super) const ANTIGRAVITY_AUTH_ALIASES: &[(&str, &str)] = &[("vertex-ai", "agent-platform")];

pub(super) struct ConfiguredAuthMethod {
    /// what the settings file holds, so an error names what the user wrote.
    pub(super) configured: String,
    /// the id `initialize` advertises for it.
    pub(super) canonical: String,
}

impl ConfiguredAuthMethod {
    pub(super) fn new(configured: String) -> Self {
        let canonical = ANTIGRAVITY_AUTH_ALIASES
            .iter()
            .find(|(alias, _)| *alias == configured)
            .map_or(configured.as_str(), |(_, canonical)| canonical)
            .to_owned();
        Self {
            configured,
            canonical,
        }
    }
}

#[derive(serde::Deserialize)]
pub(super) struct AntigravitySettings {
    #[serde(default)]
    auth: Option<AntigravityAuthSettings>,
}

#[derive(serde::Deserialize)]
pub(super) struct AntigravityAuthSettings {
    #[serde(default, rename = "type")]
    method: Option<String>,
}

pub(super) fn configured_auth_method_in(
    path: &Path,
) -> Result<Option<ConfiguredAuthMethod>, HarnessError> {
    let settings = match std::fs::read_to_string(path) {
        Ok(settings) => settings,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(HarnessError::Protocol(format!(
                "could not read Antigravity auth settings at {}: {error}",
                path.display()
            )));
        }
    };
    let settings: AntigravitySettings = deser_hjson::from_str(&settings).map_err(|error| {
        HarnessError::Protocol(format!(
            "could not parse Antigravity auth settings at {}: {error}",
            path.display()
        ))
    })?;
    Ok(settings
        .auth
        .and_then(|auth| auth.method)
        .filter(|method| !method.is_empty())
        .map(ConfiguredAuthMethod::new))
}

pub(super) fn configured_auth_method_in_home(
    gemini_home: &Path,
) -> Result<Option<ConfiguredAuthMethod>, HarnessError> {
    if let Some(method) =
        configured_auth_method_in(&gemini_home.join("antigravity-cli").join("settings.json"))?
    {
        return Ok(Some(method));
    }
    configured_auth_method_in(&gemini_home.join("antigravity-acp").join("settings.json"))
}

pub(super) fn sign_in_auth_method(
    initialized: &Value,
    default_method: &str,
    configured_method: Option<&ConfiguredAuthMethod>,
) -> Result<String, HarnessError> {
    let available: Vec<&str> = initialized
        .get("authMethods")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|method| method.get("id").and_then(Value::as_str))
        .collect();
    let selected = configured_method.map_or(default_method, |method| method.canonical.as_str());
    if available.contains(&selected) {
        return Ok(selected.to_owned());
    }
    let (source, named) = match configured_method {
        Some(method) => ("configured", method.configured.as_str()),
        None => ("default", default_method),
    };
    Err(HarnessError::Protocol(format!(
        "Antigravity's {source} auth method {named} is not advertised by the server; available methods: {}",
        available.join(", ")
    )))
}

/// one command per `<dir>/<skill>/SKILL.md`, in folder order and
/// alphabetically within each folder; the first folder wins a repeated name.
pub(super) fn skill_commands(dirs: &[PathBuf]) -> Vec<SlashCommand> {
    let mut commands: Vec<SlashCommand> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut skill_files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("SKILL.md"))
            .filter(|path| path.is_file())
            .collect();
        skill_files.sort();
        for file in skill_files {
            let Some((name, description)) = std::fs::read_to_string(&file)
                .ok()
                .and_then(|text| skill_frontmatter(&text))
            else {
                continue;
            };
            if commands.iter().any(|command| command.name == name) {
                continue;
            }
            commands.push(SlashCommand {
                name,
                description: first_sentence(&description),
                input_hint: None,
            });
        }
    }
    commands
}

/// `name` and `description` from a `SKILL.md` YAML frontmatter block,
/// including folded (`>`) or literal (`|`) multi-line descriptions.
pub(super) fn skill_frontmatter(text: &str) -> Option<(String, String)> {
    let body = text.trim_start().strip_prefix("---")?;
    let end = body.find("\n---")?;
    let lines: Vec<&str> = body[..end].lines().collect();
    let field = |key: &str| -> Option<String> {
        let index = lines.iter().position(|line| {
            line.strip_prefix(key)
                .is_some_and(|rest| rest.starts_with(':'))
        })?;
        let value = lines[index][key.len() + 1..].trim();
        let value = if matches!(value, "" | ">" | "|" | ">-" | "|-") {
            lines[index + 1..]
                .iter()
                .take_while(|line| line.starts_with(' ') || line.starts_with('\t'))
                .map(|line| line.trim())
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            value.trim_matches(|c| c == '"' || c == '\'').to_owned()
        };
        Some(value)
    };
    let name = field("name").filter(|name| !name.is_empty())?;
    Some((name, field("description").unwrap_or_default()))
}

/// skill descriptions are model-facing paragraphs; a picker row only has room
/// for the opening sentence.
pub(super) fn first_sentence(text: &str) -> String {
    let text = text.trim();
    match text.find(". ") {
        Some(end) => text[..=end].to_owned(),
        None => text.to_owned(),
    }
}

/// how long a sign-in may wait on the browser: the antigravity server gives
/// its loopback redirect 300s, plus room for the token exchange.
pub(super) const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(330);
/// sign-out is local credential removal; this bound covers the server's cold
/// start.
pub(super) const SIGN_OUT_TIMEOUT: Duration = Duration::from_secs(30);

/// milestones of [`AcpHarness::sign_in`] a caller can surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInProgress {
    /// the server is being downloaded before sign-in can start.
    /// the agent is waiting on the user at this sign-in url.
    OpenBrowser(String),
}

pub(super) fn sign_in_url(line: &str) -> Option<String> {
    let start = [line.find("https://"), line.find("http://")]
        .into_iter()
        .flatten()
        .min()?;
    let url = line[start..]
        .split_whitespace()
        .next()?
        .trim_end_matches(['.', ',', ';', ')', ']', '}']);
    reqwest::Url::parse(url).ok().map(|url| url.to_string())
}

impl AcpHarness {
    /// sign the agent out with acp `logout`, clearing the credentials its
    /// sign-in stored.
    pub async fn sign_out(&self) -> Result<(), HarnessError> {
        let home = std::env::var("HOME").ok();
        let (_scratch, mut child, _stderr) =
            self.spawn_agent(home.as_deref(), false, &[], None).await?;
        let (client, mut incoming) = match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => RpcClient::new(stdin, stdout),
            _ => {
                child.shutdown(self.kill_grace).await;
                return Err(HarnessError::Protocol("agent child has no stdio".into()));
            }
        };
        let flow = async {
            client
                .request("initialize", initialize_params(self.spec.id))
                .await?;
            request_draining(&client, &mut incoming, "logout", json!({})).await
        };
        let result = tokio::time::timeout(SIGN_OUT_TIMEOUT, flow).await;
        child.shutdown(self.kill_grace).await;
        match result {
            Ok(outcome) => outcome.map(|_| ()),
            Err(_) => Err(HarnessError::Protocol(format!(
                "{} sign-out did not finish within {}s",
                self.spec.display_name,
                SIGN_OUT_TIMEOUT.as_secs()
            ))),
        }
    }

    /// run the agent's own sign-in outside any chat: install the server if
    /// needed, then `authenticate` with the spec's method. The server opens
    /// the sign-in page through `browser` (`$BROWSER`), so a caller that opens
    /// the reported url itself can pass a no-op to avoid a second tab.
    pub async fn sign_in(
        &self,
        browser: Option<PathBuf>,
        on_progress: impl Fn(SignInProgress) + Send + Sync + 'static,
    ) -> Result<(), HarnessError> {
        let display_name = self.spec.display_name;
        let Some(default_method) = self.spec.auth_method else {
            return Err(HarnessError::Protocol(format!(
                "{display_name} has no sign-in flow"
            )));
        };
        let (exe, args) = self.resolve_program(false).await?;
        let gemini_home = (self.spec.id == HarnessId::Antigravity)
            .then(antigravity_paths::home)
            .transpose()?;
        let configured_method = gemini_home
            .as_ref()
            .map(|home| configured_auth_method_in_home(home))
            .transpose()?
            .flatten();
        let mut cmd = Command::new(&exe);
        cmd.args(args);
        crate::compose_child_path(&mut cmd, &exe);
        self.configure_adapter_environment(&mut cmd, &exe);
        if let Some(home) = std::env::var_os("HOME") {
            cmd.current_dir(home);
        }
        if let Some(home) = gemini_home {
            cmd.env("GEMINI_HOME", home);
        }
        if let Some(browser) = browser {
            cmd.env("BROWSER", browser);
        }
        let scratch = self.adapter_scratch()?;
        if let Some(dir) = &scratch {
            dir.apply(&mut cmd);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(crate::executable::binary_hint(&exe))
            } else {
                HarnessError::Io(e)
            }
        })?;
        let on_progress = std::sync::Arc::new(on_progress);
        let announced = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Some(stderr) = child.stderr.take() {
            let on_progress = on_progress.clone();
            let announced = announced.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "mast_harness::acp", "sign-in stderr: {line}");
                    if let Some(url) = sign_in_url(&line)
                        && !announced.swap(true, std::sync::atomic::Ordering::AcqRel)
                    {
                        on_progress(SignInProgress::OpenBrowser(url));
                    }
                }
            });
        }
        let (client, mut incoming) = match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => RpcClient::with_stdout_observer(
                stdin,
                stdout,
                Some(Box::new(move |line| {
                    if let Some(url) = sign_in_url(line)
                        && !announced.swap(true, std::sync::atomic::Ordering::AcqRel)
                    {
                        on_progress(SignInProgress::OpenBrowser(url));
                    }
                })),
            ),
            _ => {
                shutdown_child(&mut child, self.kill_grace).await;
                return Err(HarnessError::Protocol("agent child has no stdio".into()));
            }
        };
        let flow = async {
            let initialized = client
                .request("initialize", initialize_params(self.spec.id))
                .await?;
            let method =
                sign_in_auth_method(&initialized, default_method, configured_method.as_ref())?;
            request_draining(
                &client,
                &mut incoming,
                "authenticate",
                json!({ "methodId": method }),
            )
            .await
        };
        let result = tokio::time::timeout(SIGN_IN_TIMEOUT, flow).await;
        shutdown_child(&mut child, self.kill_grace).await;
        match result {
            Ok(outcome) => outcome.map(|_| ()),
            Err(_) => Err(HarnessError::Protocol(format!(
                "{display_name} sign-in did not finish within {} minutes",
                SIGN_IN_TIMEOUT.as_secs() / 60
            ))),
        }
    }
}
