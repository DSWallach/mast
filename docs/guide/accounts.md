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

An Antigravity login lives in a Gemini home folder. When Mast points a chat at a Mast-managed home, it sets **both** `GEMINI_HOME` (what `agy` reads) and `GEMINI_CLI_HOME` (what Gemini CLI tools read) to that one folder, so neither falls back to `~/.gemini`. Antigravity cannot own a Space account yet, because it has no in-app sign-in flow; Mast still isolates the home variables so that a Claude or Codex worker never sees an Antigravity home.

## Worker environment scrubbing

A chat that Mast starts as part of orchestrated work (sandbox **workspace-write**, with an agent token) runs its agent with a rebuilt environment, not a copy of yours. Only these reach it:

- the basics a process needs (`PATH`, `HOME`, `USER`, locale, `TERM`, `TMPDIR`, `XDG_*`, proxy and certificate settings, `SSH_AUTH_SOCK`);
- toolchain settings (`CARGO_*`, `RUSTUP_*`, `RUST*`, `NODE_*`, `NPM_CONFIG_*`, `PYTHON*`, `GOPATH`, `JAVA_HOME` and similar), unless the name looks like a secret (`TOKEN`, `SECRET`, `PASSWORD`, `API_KEY`);
- the agent's own settings (`ANTHROPIC_*` and `CLAUDE_*` for Claude Code, `OPENAI_*` and `CODEX_*` for Codex, and so on);
- `MAST_*` settings, and the chat's own scoped `MAST_AGENT_TOKEN`;
- its single account home (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, or the Gemini pair).

Everything else is dropped, including other providers' API keys, other accounts' home folders, an inherited `MAST_AGENT_TOKEN`, raw OAuth refresh tokens (`*REFRESH_TOKEN*`, always, even for the agent's own provider), and general secrets such as `GH_TOKEN`, `AWS_*` and `NPM_TOKEN`. On a Space account, the agent's own API-key variables (such as `ANTHROPIC_API_KEY`) are dropped too, so a key in your shell cannot override the account's login. Chats on the Default account keep their own variables.

Scrubbing applies to workspace-write runs with an agent token. Read-only and full-access runs, and ordinary chats without one, are unchanged. Mast cannot tell a Manager from a worker at this point, so Manager and Lead chats are scrubbed the same way.

If a tool in a worker needs a variable that is dropped, list its name in `MAST_WORKER_ENV_PASSTHROUGH` (comma or space separated) in the environment Mast runs in. A listed variable is passed through, except `MAST_AGENT_TOKEN` and refresh tokens, which are never passed.

Scrubbing hides secrets carried in the environment. It does not stop a worker from reading files your user can read; the sandbox decides that.

## Try it

1. Add an account to one Space and sign in with a different login.
2. Start a chat in that Space. Check the account chip in the chat header.
3. Start a chat in another Space and check that it shows Default.
4. Run both at once.
