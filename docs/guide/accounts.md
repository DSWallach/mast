# Accounts

**Default** uses the same Claude Code and Codex logins as the CLIs on your machine. A Space can use its own account instead, so work and personal projects stay apart, and two accounts can run at the same time.

## How it works

- Each Space has a default account per agent (Claude Code, Codex).
- A chat picks the Space's default when it first runs and keeps it. Change the default and only new chats change. Chats that already have an agent session stay on Default.
- A Space account has its own home folder under Mast's data directory. It does not touch `<home>/.claude` or `<home>/.codex`. Skills and rules are shared into it, but credentials are not.
- The chat header shows which account a chat uses. The Start Chat dialog has an account chip for a one-chat override (it only shows when you have a Space account).

## Add an account

1. Right-click the Space and choose its account entry (it reads like "Claude account: Default…"), then **Add account…**.
2. Name it, and click **Continue**.
3. Mast starts the agent's sign-in (`claude auth login` or `codex login`). Click **Open** to see the page in your browser, or **Copy** the link. If the browser shows a code, paste it into the field and click **Submit**. Mast does not store the code.
4. When it says it is done, the account is the Space's default for that agent.

**Cancel** stops the sign-in.

## Saved accounts

Logins you already added under **Settings → Accounts** show up in the Space account picker as saved logins. Choosing one moves it into a new Space account. Two limits:

- The login you currently use as Default cannot be moved. Use **Add account** to sign in again.
- A moved login leaves the global switcher, and Mast stops refreshing it there, so two copies never fight over one token.

## Troubleshooting a first chat

If the selected account is signed out, the chat preflight offers **Sign in…** or **Start anyway**. For Default, sign in through **Settings → Accounts**; for a Space account, use the **Space accounts** section on that page and **Sign in again**. Check the chat header for its pinned account. `mast doctor` checks local prerequisites and suggests fixes.

## Usage

The **Space accounts** section of **Settings → Accounts** lists each account with its usage windows.

### Per-model windows

Besides the 5-hour **Session** and **Week** windows, an account shows every other window its provider reports, each under its own label:

- **Claude Code**: a weekly window per model family (for example **Opus Week**, **Sonnet Week**, or a Fable week), **Week (overage included)**, and **Extra usage** (your credit budget, as a fraction spent). A bucket the provider leaves empty is not shown. **Extra usage** appears only next to at least one rate window.
- **Antigravity**: **Session** and **Week** for the account, plus a window for every model group (Carbon, Neon, Hydrogen, Argon, Flash). Groups that share a bucket name are labelled with the group, such as "Gemini 4 Carbon · Five Hour Limit Remaining".

### When a chat hits a limit

Chats on Antigravity, Grok, Devin, Hermes, Pi and opencode tag a quota error the same way Claude Code and Codex do: **5-hour limit**, **weekly limit** (a weekly limit that names one model is treated as that model's own), a general **usage limit** (out of credits or plan usage), or a **temporary rate limit**. A temporary rate limit never reads as out of credits: its message says the account is being throttled and to retry shortly, so Mast does not offer to switch your default model for it. Reset times are not known for these agents.

## Antigravity account homes

An Antigravity login lives in a Gemini home folder (`~/.gemini` by default). When Mast points a chat at a Mast-managed home, it sets **both** `GEMINI_HOME` (the name Mast's own Antigravity setup reads) and `GEMINI_CLI_HOME` to that one folder.

Know the limit: the `agy` command-line program itself (checked with the version installed on 2026-11-27) ignores both names and keeps its state under `~/.gemini` in the user's home, so these variables alone do not separate two Antigravity logins. Antigravity cannot own a Space account yet, because it has no in-app sign-in flow. What does work today: a Claude or Codex worker never sees an Antigravity home variable.

## Worker environment scrubbing

A chat that Mast starts as a task worker (a child chat with an implement, test or docs task) and runs in **workspace-write** mode runs its agent with a rebuilt environment, not a copy of yours. Only these reach it:

- the basics a process needs (`PATH`, `HOME`, `USER`, locale, `TERM`, `TMPDIR`, `XDG_*`);
- network settings: the proxy variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, lower-case too) and certificate settings (`SSL_CERT_FILE`, `SSL_CERT_DIR`, `NODE_EXTRA_CA_CERTS`);
- `SSH_AUTH_SOCK`;
- toolchain settings (`CARGO_*`, `RUSTUP_*`, `RUST*`, `NODE_*`, `NPM_CONFIG_*`, `PYTHON*`, `GOPATH`, `JAVA_HOME` and similar), unless the name looks like a secret (`TOKEN`, `SECRET`, `PASSWORD`, `API_KEY`);
- the agent's own settings (`ANTHROPIC_*` and `CLAUDE_*` for Claude Code, `OPENAI_*` and `CODEX_*` for Codex, `CURSOR_*` for Cursor, and so on);
- `MAST_*` settings, and the chat's own scoped `MAST_AGENT_TOKEN`;
- its single account home (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, or the Gemini pair).

Everything else is dropped, including other providers' API keys, other accounts' home folders, an inherited `MAST_AGENT_TOKEN`, raw OAuth refresh tokens (`*REFRESH_TOKEN*`, always, even for the agent's own provider), and general secrets such as `GH_TOKEN`, `AWS_*` and `NPM_TOKEN`. On a Space account, known login overrides such as `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, `OPENAI_API_KEY`, `CODEX_API_KEY`, `GEMINI_API_KEY`, and `GOOGLE_API_KEY` are dropped by default, unless explicitly listed in the passthrough below. Other variables in the agent's own namespace may still pass, so check the environment for other login overrides. Chats on the Default account keep their own variables. This covers Claude Code, Codex, Cursor, opencode and the ACP agents (Antigravity, Grok, Devin, Hermes, Pi).

Managers, Leads, reviewers, admin and assistant chats, parentless chats (including those with a task assigned), ordinary chats you start yourself, and read-only or full-access runs are not scrubbed and keep your environment.

### What is kept on purpose

- **Proxy and certificate variables** stay because a worker behind a corporate proxy cannot reach its model provider or fetch dependencies without them. The catch: a proxy URL can carry a username and password (`http://user:pass@host`), and the worker can read it.
- **`SSH_AUTH_SOCK`** stays so a worker's `git` and `ssh` keep working against your agent. The catch: a worker can use every key loaded in that agent, as you, for as long as the run lives. Workers do not merge, but they could push.

To opt out of either, start Mast with those variables unset (this affects Mast's own network use too) or, for the agent-socket case, with `SSH_AUTH_SOCK` pointing at an agent that holds only the keys workers should have. To go the other way and let a worker see a variable the scrub drops, list its name in `MAST_WORKER_ENV_PASSTHROUGH` (comma or space separated) in the environment Mast runs in. It is **off by default** (unset means nothing extra passes), and only the names you list pass. An inherited `MAST_AGENT_TOKEN`, refresh tokens, and the scrub control variables are never passed through, even if listed; Mast injects the chat’s scoped token separately.

Scrubbing hides secrets carried in the environment. It does not stop a worker from reading files your user can read (for example the Default account's login file under `~/.claude`); the sandbox decides that.

## Try it

1. Add an account to one Space and sign in with a different login.
2. Start a chat in that Space. Check the account chip in the chat header.
3. Start a chat in another Space and check that it shows Default.
4. Run both at once.
