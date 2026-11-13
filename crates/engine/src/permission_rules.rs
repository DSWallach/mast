//! User permission rules from `~/.mast/settings.json`, in Claude Code's
//! `permissions.{allow,deny,ask}` format. Harnesses describe a request as a
//! [`PermissionRequest`]; this module decides it. Anything it cannot read
//! exactly is `Ask`, never `Allow`.

use crate::EngineError;
use crate::approval_commands::{normalize_segment, parse_command};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

pub use mast_proto::PermissionRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleDecision {
    /// The allow rules that covered the request, one per command segment.
    Allow(Vec<String>),
    Deny(String),
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pattern {
    /// `Bash(git log:*)`: these words, then anything.
    BashPrefix(Vec<String>),
    /// `Bash(npm test)`: exactly these words.
    BashExact(Vec<String>),
    /// `Bash(git * main)`: `*` matches any text.
    BashGlob(String),
    /// `mcp__server`, `mcp__server__*` (tool `None`) or `mcp__server__tool`.
    Mcp {
        server: String,
        tool: Option<String>,
    },
    /// A bare tool name such as `WebSearch`.
    Tool(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    text: String,
    pattern: Pattern,
}

/// One parsed settings file.
#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    allow: Vec<Rule>,
    deny: Vec<Rule>,
    ask: Vec<Rule>,
    aliases: Vec<(String, String)>,
    /// Rules this version cannot evaluate, kept for the settings page.
    pub unsupported: Vec<String>,
    /// Tools named by a deny or ask rule Mast can't evaluate; requests for
    /// them always ask rather than risk skipping the rule.
    guarded_tools: Vec<String>,
    /// Why the file could not be used, if it exists but failed to parse.
    pub error: Option<String>,
}

impl RuleSet {
    pub fn parse(text: &str) -> Self {
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(err) => {
                return Self {
                    error: Some(format!("Invalid JSON: {err}")),
                    ..Self::default()
                };
            }
        };
        let mut set = Self::default();
        let permissions = value.get("permissions");
        for (key, list) in [
            ("allow", &mut set.allow),
            ("deny", &mut set.deny),
            ("ask", &mut set.ask),
        ] {
            for text in permissions
                .and_then(|p| p.get(key))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                match parse_rule(text) {
                    Some(pattern) => list.push(Rule {
                        text: text.to_owned(),
                        pattern,
                    }),
                    None => {
                        if key != "allow" {
                            let tool = text.split('(').next().unwrap_or(text).trim();
                            set.guarded_tools.push(tool.to_owned());
                        }
                        set.unsupported.push(text.to_owned());
                    }
                }
            }
        }
        if let Some(map) = value.get("mcpServerAliases").and_then(Value::as_object) {
            for (name, canonical) in map {
                if let Some(canonical) = canonical.as_str() {
                    set.aliases
                        .push((name.to_owned(), server_key(canonical, &[])));
                }
            }
        }
        set
    }

    pub fn allow_rules(&self) -> Vec<String> {
        self.allow.iter().map(|r| r.text.clone()).collect()
    }
    pub fn deny_rules(&self) -> Vec<String> {
        self.deny.iter().map(|r| r.text.clone()).collect()
    }
    pub fn ask_rules(&self) -> Vec<String> {
        self.ask.iter().map(|r| r.text.clone()).collect()
    }

    /// Deny beats ask, ask beats allow, and a command needs an allow rule for
    /// every segment.
    pub fn decide(&self, request: &PermissionRequest) -> RuleDecision {
        if self.error.is_some() {
            return RuleDecision::Ask;
        }
        let tool = match request {
            PermissionRequest::Command { .. } => Some("Bash"),
            PermissionRequest::Tool { name } => Some(name.as_str()),
            _ => None,
        };
        if tool.is_some_and(|t| self.guarded_tools.iter().any(|g| g == t)) {
            return RuleDecision::Ask;
        }
        match request {
            PermissionRequest::Command { command } => self.decide_command(command),
            PermissionRequest::McpTool { server, tool } => {
                let hit = |rules: &[Rule]| {
                    rules
                        .iter()
                        .find(|r| self.matches_mcp(&r.pattern, server, tool))
                        .map(|r| r.text.clone())
                };
                if let Some(rule) = hit(&self.deny) {
                    return RuleDecision::Deny(rule);
                }
                if hit(&self.ask).is_some() {
                    return RuleDecision::Ask;
                }
                hit(&self.allow).map_or(RuleDecision::Ask, |r| RuleDecision::Allow(vec![r]))
            }
            PermissionRequest::Tool { name } | PermissionRequest::McpToolJoined { name } => {
                let joined = matches!(request, PermissionRequest::McpToolJoined { .. });
                let hit = |rules: &[Rule]| {
                    rules
                        .iter()
                        .find(|r| {
                            if joined {
                                self.matches_joined(&r.pattern, name)
                            } else {
                                r.pattern == Pattern::Tool(name.clone())
                            }
                        })
                        .map(|r| r.text.clone())
                };
                if let Some(rule) = hit(&self.deny) {
                    return RuleDecision::Deny(rule);
                }
                if hit(&self.ask).is_some() {
                    return RuleDecision::Ask;
                }
                hit(&self.allow).map_or(RuleDecision::Ask, |r| RuleDecision::Allow(vec![r]))
            }
        }
    }

    fn decide_command(&self, command: &Value) -> RuleDecision {
        let Some(segments) = parse_command(command) else {
            // A deny rule on the whole text still applies to unparseable scripts.
            return self
                .deny
                .iter()
                .find(|r| command_text(command).is_some_and(|t| text_matches(&r.pattern, &t)))
                .map_or(RuleDecision::Ask, |r| RuleDecision::Deny(r.text.clone()));
        };
        let mut words = Vec::new();
        for segment in &segments {
            let Some((assignments, argv)) = normalize_segment(segment) else {
                return RuleDecision::Ask;
            };
            if let Some(rule) = self.deny.iter().find(|r| argv_matches(&r.pattern, &argv)) {
                return RuleDecision::Deny(rule.text.clone());
            }
            words.push((assignments, argv));
        }
        let mut allowed = Vec::new();
        for (assignments, argv) in &words {
            if !assignments.is_empty() || hard_stop(argv) {
                return RuleDecision::Ask;
            }
            // `git -C x push` must not slip past a `git push` deny rule.
            if self
                .ask
                .iter()
                .chain(&self.deny)
                .any(|r| argv_matches(&r.pattern, argv) || near_miss(&r.pattern, argv))
            {
                return RuleDecision::Ask;
            }
            match self.allow.iter().find(|r| argv_matches(&r.pattern, argv)) {
                Some(rule) => allowed.push(rule.text.clone()),
                None => return RuleDecision::Ask,
            }
        }
        if allowed.is_empty() {
            return RuleDecision::Ask;
        }
        allowed.dedup();
        RuleDecision::Allow(allowed)
    }

    fn matches_mcp(&self, pattern: &Pattern, server: &str, tool: &str) -> bool {
        let Pattern::Mcp {
            server: rule_server,
            tool: rule_tool,
        } = pattern
        else {
            return false;
        };
        server_key(rule_server, &self.aliases) == server_key(server, &self.aliases)
            && rule_tool.as_deref().is_none_or(|t| t == tool)
    }

    /// OpenCode names an MCP tool `<server>_<tool>` with other characters
    /// replaced by `_`, so compare in that form.
    fn matches_joined(&self, pattern: &Pattern, name: &str) -> bool {
        let Pattern::Mcp { server, tool } = pattern else {
            return false;
        };
        let fold = |s: &str| {
            s.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '_'
                    }
                })
                .collect::<String>()
        };
        let server = fold(&server_key(server, &self.aliases));
        let name = fold(name);
        match tool {
            Some(tool) => name == format!("{server}_{}", fold(tool)),
            // `notion_x` could be server `notion` or `notion_x`; never guess.
            None => false,
        }
    }
}

/// Names one MCP server the way every provider spells it: Claude's
/// `claude_ai_Notion` and `plugin_engineering_notion` and Codex's `notion`
/// all become `notion`. Explicit aliases win.
fn server_key(name: &str, aliases: &[(String, String)]) -> String {
    if let Some((_, canonical)) = aliases.iter().find(|(alias, _)| alias == name) {
        return canonical.clone();
    }
    let lower = name.to_ascii_lowercase().replace([' ', '-'], "_");
    let stripped = lower.strip_prefix("claude_ai_").unwrap_or(&lower);
    let stripped = stripped
        .strip_prefix("plugin_")
        .and_then(|rest| rest.split_once('_').map(|(_, server)| server))
        .unwrap_or(stripped);
    stripped.to_owned()
}

fn parse_rule(text: &str) -> Option<Pattern> {
    let text = text.trim();
    if let Some(inner) = text.strip_prefix("Bash(").and_then(|t| t.strip_suffix(')')) {
        let inner = inner.trim();
        if inner.is_empty() {
            return None;
        }
        if let Some(prefix) = inner.strip_suffix(":*") {
            let words = shell_words::split(prefix).ok()?;
            return (!words.is_empty()).then_some(Pattern::BashPrefix(words));
        }
        if inner.contains('*') {
            return Some(Pattern::BashGlob(inner.to_owned()));
        }
        let words = shell_words::split(inner).ok()?;
        return (!words.is_empty()).then_some(Pattern::BashExact(words));
    }
    if text == "Bash" {
        return None;
    }
    if let Some(rest) = text.strip_prefix("mcp__") {
        let (server, tool) = match rest.split_once("__") {
            Some((server, "*")) => (server, None),
            Some((server, tool)) if !tool.is_empty() => (server, Some(tool.to_owned())),
            Some(_) => return None,
            None => (rest, None),
        };
        return (!server.is_empty()).then(|| Pattern::Mcp {
            server: server.to_owned(),
            tool,
        });
    }
    // `Edit(src/**)`, `WebFetch(domain:x)` and other argument rules need the
    // provider's own matcher; only Claude's CLI can apply them.
    if text.contains('(') || text.is_empty() {
        return None;
    }
    text.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
        .then(|| Pattern::Tool(text.to_owned()))
}

fn argv_matches(pattern: &Pattern, argv: &[String]) -> bool {
    match pattern {
        Pattern::BashPrefix(words) => argv.starts_with(words),
        Pattern::BashExact(words) => argv == words.as_slice(),
        Pattern::BashGlob(glob) => glob_matches(glob, &shell_words::join(argv)),
        _ => false,
    }
}

/// The rule's program with its other words appearing in order, possibly
/// separated by options (`git -C x push` against `git push`).
fn near_miss(pattern: &Pattern, argv: &[String]) -> bool {
    let words = match pattern {
        Pattern::BashPrefix(words) | Pattern::BashExact(words) => words,
        _ => return false,
    };
    if words.first() != argv.first() {
        return false;
    }
    let mut rest = argv.iter().skip(1);
    words.iter().skip(1).all(|word| rest.any(|arg| arg == word))
}

fn text_matches(pattern: &Pattern, text: &str) -> bool {
    match pattern {
        Pattern::BashPrefix(words) => {
            let prefix = shell_words::join(words);
            text == prefix || text.starts_with(&format!("{prefix} "))
        }
        Pattern::BashExact(words) => text == shell_words::join(words),
        Pattern::BashGlob(glob) => glob_matches(glob, text),
        _ => false,
    }
}

fn glob_matches(glob: &str, text: &str) -> bool {
    let pattern = glob
        .split('*')
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join(".*");
    regex::Regex::new(&format!("^{pattern}$")).is_ok_and(|re| re.is_match(text))
}

fn command_text(command: &Value) -> Option<String> {
    match command {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => parts
            .iter()
            .map(|p| p.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .map(|argv| shell_words::join(argv)),
        _ => None,
    }
}

/// Never answered by a rule, whatever the settings say: the rules file itself,
/// and privilege or recursive-delete commands.
fn hard_stop(argv: &[String]) -> bool {
    let program = Path::new(&argv[0])
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    program == "sudo"
        || (program == "rm"
            && argv.iter().skip(1).any(|a| {
                a == "--recursive"
                    || (a.starts_with('-') && !a.starts_with("--") && a.contains(['r', 'R']))
            }))
        || argv.iter().any(|a| touches_rules_file(a))
}

/// The rules file, however its path is spelled (`~/.mast/./settings.json`).
pub fn touches_rules_file(text: &str) -> bool {
    text.contains(".mast") && text.contains("settings")
}

/// Programs that run other programs: a prefix rule on them allows anything.
fn runs_other_programs(program: &str) -> bool {
    matches!(
        program,
        "sh" | "bash"
            | "zsh"
            | "fish"
            | "dash"
            | "ksh"
            | "env"
            | "xargs"
            | "nohup"
            | "exec"
            | "eval"
            | "time"
            | "nice"
            | "timeout"
            | "watch"
            | "sudo"
            | "python"
            | "python3"
            | "node"
            | "ruby"
            | "perl"
            | "deno"
            | "bun"
            | "npx"
            | "osascript"
    )
}

/// Suggest an allow rule for Always allow. Commands get the program plus its
/// first subcommand word; MCP tools get the exact tool.
pub fn suggest_rule(request: &PermissionRequest) -> Option<String> {
    match request {
        PermissionRequest::McpTool { server, tool } => Some(format!("mcp__{server}__{tool}")),
        PermissionRequest::Tool { name } => Some(name.clone()),
        // The joined name can't be split into server and tool safely.
        PermissionRequest::McpToolJoined { .. } => None,
        PermissionRequest::Command { command } => {
            let segments = parse_command(command)?;
            let [segment] = segments.as_slice() else {
                return None;
            };
            let (assignments, argv) = normalize_segment(segment)?;
            if !assignments.is_empty() || hard_stop(&argv) {
                return None;
            }
            let program = Path::new(&argv[0])
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if runs_other_programs(program) {
                return None;
            }
            // `git -C x status` would widen to all of git; offer just this command.
            if argv.get(1).is_some_and(|w| w.starts_with('-')) {
                return Some(format!("Bash({})", shell_words::join(&argv)));
            }
            let mut prefix = vec![argv[0].clone()];
            if let Some(sub) = argv.get(1).filter(|w| {
                !w.starts_with('-') && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            }) {
                prefix.push(sub.clone());
            }
            Some(format!("Bash({}:*)", shell_words::join(prefix)))
        }
    }
}

/// Validate a rule typed in the Always allow editor and say whether it covers
/// the request.
pub fn rule_covers(rule: &str, request: &PermissionRequest) -> Result<bool, String> {
    if parse_rule(rule).is_none() {
        return Err(
            "Mast can't check this kind of rule. Use Bash(...), mcp__server__tool or a tool name."
                .into(),
        );
    }
    let set = RuleSet {
        allow: vec![Rule {
            text: rule.to_owned(),
            pattern: parse_rule(rule).expect("checked above"),
        }],
        ..RuleSet::default()
    };
    Ok(matches!(set.decide(request), RuleDecision::Allow(_)))
}

/// The settings file, re-read whenever it changes on disk.
pub struct PermissionRules {
    path: PathBuf,
    cache: Mutex<Option<(Option<SystemTime>, u64, Arc<RuleSet>)>>,
    write: Mutex<()>,
}

impl PermissionRules {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cache: Mutex::new(None),
            write: Mutex::new(()),
        }
    }

    pub fn default_path() -> PathBuf {
        crate::repos::home_dir().join(".mast").join("settings.json")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn current(&self) -> Arc<RuleSet> {
        let meta = std::fs::metadata(&self.path).ok();
        let stamp = meta.as_ref().and_then(|m| m.modified().ok());
        let len = meta.as_ref().map_or(0, |m| m.len());
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((cached_stamp, cached_len, set)) = cache.as_ref() {
            if *cached_stamp == stamp && *cached_len == len {
                return set.clone();
            }
        }
        let set = Arc::new(match std::fs::read_to_string(&self.path) {
            Ok(text) => RuleSet::parse(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => RuleSet::default(),
            Err(err) => RuleSet {
                error: Some(format!("Can't read {}: {err}", self.path.display())),
                ..RuleSet::default()
            },
        });
        *cache = Some((stamp, len, set.clone()));
        set
    }

    pub fn decide(&self, request: &PermissionRequest) -> RuleDecision {
        self.current().decide(request)
    }

    /// Add `rule` to `permissions.allow`, keeping every other key and the
    /// existing order. A file that doesn't parse is never overwritten.
    pub fn add_allow(&self, rule: &str) -> Result<(), EngineError> {
        self.edit(|allow| {
            if !allow.iter().any(|r| r.as_str() == Some(rule)) {
                allow.push(Value::String(rule.to_owned()));
            }
        })
    }

    pub fn remove_allow(&self, rule: &str) -> Result<(), EngineError> {
        self.edit(|allow| allow.retain(|r| r.as_str() != Some(rule)))
    }

    fn edit(&self, change: impl FnOnce(&mut Vec<Value>)) -> Result<(), EngineError> {
        let _guard = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let mut root = match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str::<Value>(&text).map_err(|err| {
                EngineError::Other(format!(
                    "{} isn't valid JSON ({err}); fix it before saving a rule",
                    self.path.display()
                ))
            })?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
            Err(err) => return Err(err.into()),
        };
        let root_obj = root
            .as_object_mut()
            .ok_or_else(|| EngineError::Other("settings.json must be a JSON object".into()))?;
        let permissions = root_obj
            .entry("permissions")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or_else(|| EngineError::Other("\"permissions\" must be an object".into()))?;
        let allow = permissions
            .entry("allow")
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| EngineError::Other("\"permissions.allow\" must be a list".into()))?;
        change(allow);
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut bytes =
            serde_json::to_vec_pretty(&root).map_err(|e| EngineError::Other(e.to_string()))?;
        bytes.push(b'\n');
        crate::agent_accounts::write_file_atomic(&self.path, &bytes, false)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set(allow: &[&str], deny: &[&str], ask: &[&str]) -> RuleSet {
        RuleSet::parse(
            &json!({"permissions": {"allow": allow, "deny": deny, "ask": ask}}).to_string(),
        )
    }
    fn cmd(script: &str) -> PermissionRequest {
        PermissionRequest::Command {
            command: json!(script),
        }
    }
    fn mcp(server: &str, tool: &str) -> PermissionRequest {
        PermissionRequest::McpTool {
            server: server.into(),
            tool: tool.into(),
        }
    }

    #[test]
    fn bash_prefix_exact_and_glob_rules() {
        let rules = set(
            &[
                "Bash(/Users/d/bin/gh_read:*)",
                "Bash(git log:*)",
                "Bash(npm test)",
                "Bash(git diff * main)",
            ],
            &[],
            &[],
        );
        let allow = |s: &str| matches!(rules.decide(&cmd(s)), RuleDecision::Allow(_));
        assert!(allow("/Users/d/bin/gh_read pulls 12"));
        assert!(allow("git log --oneline -5"));
        assert!(allow("git log"));
        assert!(allow("npm test"));
        assert!(!allow("npm test --watch"));
        assert!(allow("git diff HEAD~1 main"));
        assert!(!allow("git logx"));
        assert!(!allow("git status"));
    }

    #[test]
    fn every_segment_needs_its_own_rule() {
        let rules = set(&["Bash(git log:*)", "Bash(head:*)"], &[], &[]);
        assert_eq!(
            rules.decide(&cmd("git log | head -5")),
            RuleDecision::Allow(vec!["Bash(git log:*)".into(), "Bash(head:*)".into()])
        );
        assert_eq!(rules.decide(&cmd("git log && rm x")), RuleDecision::Ask);
        assert_eq!(rules.decide(&cmd("git log > out")), RuleDecision::Ask);
        assert_eq!(rules.decide(&cmd("git log $(whoami)")), RuleDecision::Ask);
        assert_eq!(rules.decide(&cmd("FOO=1 git log")), RuleDecision::Ask);
    }

    #[test]
    fn codex_shell_wrapper_is_unwrapped() {
        let rules = set(&["Bash(git log:*)"], &[], &[]);
        let request = PermissionRequest::Command {
            command: json!(["/bin/zsh", "-lc", "git log -3"]),
        };
        assert!(matches!(rules.decide(&request), RuleDecision::Allow(_)));
    }

    #[test]
    fn deny_beats_ask_beats_allow() {
        let rules = set(
            &["Bash(git:*)", "mcp__notion"],
            &["Bash(git push:*)", "mcp__notion__notion-delete-page"],
            &["Bash(git reset:*)"],
        );
        assert_eq!(
            rules.decide(&cmd("git status && git push")),
            RuleDecision::Deny("Bash(git push:*)".into())
        );
        assert_eq!(rules.decide(&cmd("git reset --hard")), RuleDecision::Ask);
        assert!(matches!(
            rules.decide(&cmd("git status")),
            RuleDecision::Allow(_)
        ));
        assert_eq!(
            rules.decide(&mcp("notion", "notion-delete-page")),
            RuleDecision::Deny("mcp__notion__notion-delete-page".into())
        );
        assert!(matches!(
            rules.decide(&mcp("notion", "notion-fetch")),
            RuleDecision::Allow(_)
        ));
    }

    #[test]
    fn mcp_server_names_match_across_providers() {
        let rules = RuleSet::parse(
            &json!({
                "permissions": {"allow": [
                    "mcp__claude_ai_Notion__notion-update-page",
                    "mcp__plugin_engineering_linear__get_issue",
                    "mcp__82b0abf1-2c4c__notion-fetch",
                    "mcp__slack__*"
                ]},
                "mcpServerAliases": {"82b0abf1-2c4c": "notion"}
            })
            .to_string(),
        );
        let allow = |s: &str, t: &str| matches!(rules.decide(&mcp(s, t)), RuleDecision::Allow(_));
        assert!(allow("notion", "notion-update-page"));
        assert!(allow("Notion", "notion-update-page"));
        assert!(allow("linear", "get_issue"));
        assert!(allow("notion", "notion-fetch"));
        assert!(allow("slack", "slack_send_message"));
        assert!(!allow("notion", "notion-create-pages"));
        assert!(!allow("jam", "notion-update-page"));
        let joined = |name: &str| {
            matches!(
                rules.decide(&PermissionRequest::McpToolJoined { name: name.into() }),
                RuleDecision::Allow(_)
            )
        };
        assert!(joined("notion_notion-update-page"));
        // Server-wide rules can't vouch for a name that may belong to another server.
        assert!(!joined("slack_slack_send_message"));
        assert!(!joined("notion_notion-create-pages"));
        assert!(!joined("notionx_notion-update-page"));
    }

    #[test]
    fn review_gaps_stay_closed() {
        let rules = set(
            &["Bash(git:*)", "Bash(rm:*)", "Edit", "mcp__github"],
            &["Bash(git push:*)", "Edit(~/.ssh/**)"],
            &[],
        );
        for script in [
            "git -C . push",
            "git --no-pager push origin",
            "git -c x=y push",
            "rm -Rf build",
            "rm -fR build",
            "rm -r -f build",
            "rm --recursive build",
            "git add /Users/d/.mast/./settings.json",
        ] {
            assert_eq!(rules.decide(&cmd(script)), RuleDecision::Ask, "{script}");
        }
        assert!(matches!(
            rules.decide(&cmd("git status")),
            RuleDecision::Allow(_)
        ));
        // An Edit deny rule Mast can't evaluate keeps every Edit asking.
        assert_eq!(
            rules.decide(&PermissionRequest::Tool {
                name: "Edit".into()
            }),
            RuleDecision::Ask
        );
        // A server-wide rule can't vouch for an OpenCode name it can't split.
        assert_eq!(
            rules.decide(&PermissionRequest::McpToolJoined {
                name: "github_admin_delete".into()
            }),
            RuleDecision::Ask
        );
    }

    #[test]
    fn suggestions_never_widen_to_shells_or_flags() {
        assert_eq!(suggest_rule(&cmd("python3 -c 'print(1)'")), None);
        assert_eq!(suggest_rule(&cmd("env FOO=1 ls")), None);
        assert_eq!(
            suggest_rule(&cmd("git -C repo status")).unwrap(),
            "Bash(git -C repo status)"
        );
        let request = PermissionRequest::Command {
            command: json!(["/opt/homebrew/bin/zsh", "-lc", "ls"]),
        };
        assert_eq!(suggest_rule(&request), None);
    }

    #[test]
    fn codex_escalation_string_is_unwrapped() {
        let rules = set(&["Bash(/Users/d/bin/gh_read:*)"], &[], &[]);
        assert!(matches!(
            rules.decide(&cmd("/bin/zsh -lc '/Users/d/bin/gh_read pulls 12'")),
            RuleDecision::Allow(_)
        ));
        assert_eq!(
            rules.decide(&cmd("/bin/zsh -lc '/Users/d/bin/gh_read x; curl evil'")),
            RuleDecision::Ask
        );
    }

    #[test]
    fn hard_stops_and_broken_files_never_allow() {
        let rules = set(&["Bash(sudo:*)", "Bash(rm:*)", "Bash(cat:*)"], &[], &[]);
        assert_eq!(rules.decide(&cmd("sudo ls")), RuleDecision::Ask);
        assert_eq!(rules.decide(&cmd("rm -rf build")), RuleDecision::Ask);
        assert!(matches!(
            rules.decide(&cmd("rm build/x")),
            RuleDecision::Allow(_)
        ));
        assert_eq!(
            rules.decide(&cmd("cat /Users/d/.mast/settings.json")),
            RuleDecision::Ask
        );
        let broken = RuleSet::parse("{ not json");
        assert!(broken.error.is_some());
        assert_eq!(broken.decide(&cmd("ls")), RuleDecision::Ask);
    }

    #[test]
    fn unsupported_rules_are_reported_not_applied() {
        let rules = set(
            &[
                "Edit(src/**)",
                "WebFetch(domain:x.com)",
                "WebSearch",
                "Bash()",
            ],
            &[],
            &[],
        );
        assert_eq!(
            rules.unsupported,
            ["Edit(src/**)", "WebFetch(domain:x.com)", "Bash()"]
        );
        assert!(matches!(
            rules.decide(&PermissionRequest::Tool {
                name: "WebSearch".into()
            }),
            RuleDecision::Allow(_)
        ));
    }

    #[test]
    fn suggestions_cover_their_request() {
        for request in [
            cmd("/Users/d/bin/gh_read pulls 12"),
            cmd("git log --oneline"),
            mcp("notion", "notion-update-page"),
        ] {
            let rule = suggest_rule(&request).unwrap();
            assert_eq!(rule_covers(&rule, &request), Ok(true), "{rule}");
        }
        assert_eq!(
            suggest_rule(&cmd("git log --oneline")).unwrap(),
            "Bash(git log:*)"
        );
        assert_eq!(suggest_rule(&cmd("git log | head")), None);
        assert_eq!(suggest_rule(&cmd("sudo ls")), None);
        assert!(rule_covers("Edit(x)", &cmd("ls")).is_err());
    }

    #[test]
    fn add_and_remove_keep_other_keys_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mast/settings.json");
        let rules = PermissionRules::new(path.clone());
        assert_eq!(rules.decide(&cmd("git log")), RuleDecision::Ask);
        rules.add_allow("Bash(git log:*)").unwrap();
        rules.add_allow("Bash(git log:*)").unwrap();
        assert!(matches!(
            rules.decide(&cmd("git log")),
            RuleDecision::Allow(_)
        ));
        let text = std::fs::read_to_string(&path).unwrap();
        let mut value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["permissions"]["allow"], json!(["Bash(git log:*)"]));
        value["zzz"] = json!(1);
        value["permissions"]["deny"] = json!(["Bash(git log --all:*)"]);
        std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
        rules.add_allow("mcp__notion__notion-fetch").unwrap();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["zzz"], json!(1));
        assert_eq!(
            rules.decide(&cmd("git log --all")),
            RuleDecision::Deny("Bash(git log --all:*)".into())
        );
        rules.remove_allow("Bash(git log:*)").unwrap();
        assert_eq!(rules.decide(&cmd("git log")), RuleDecision::Ask);
        std::fs::write(&path, "{ broken").unwrap();
        assert!(rules.add_allow("Bash(ls:*)").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ broken");
    }
}
