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

## Cap modes (upcoming)

Cap modes are not active in Mast yet. The quota broker reads usage when this section or an agent's `quota_status` asks for it, but Mast does not yet use it to move work, pause, freeze, save a checkpoint or send a notification. You can choose the mode and caps now; they are saved and take effect once quota-aware dispatch is on.

### Set the mode and caps

Open **Settings → Accounts** and find **Quota caps**, below the accounts. Caps are how much of a plan dispatched work may use; the rest stays free for your interactive chats.

- **Mode**: **Balanced failover** or **Pinned freeze**.
- **Soft cap** and **Freeze cap**: adjust each in 1% steps. The defaults are 78% and 90%, shared by every provider.
- **Hard cap**: each provider (Claude Code, Codex, Antigravity) has its own slider, from 50% to 95% in 5% notches. Click a notch to set it. The default is 85%. A provider you don't move keeps the default.
- **Reset to defaults** puts Balanced failover and the default caps back in the controls, and removes every provider's own hard cap. Click **Save** to keep them.

Each provider's hard cap must stay above the soft cap and below the freeze cap. While it doesn't, the section names the provider, says why, and **Save** is disabled.

### Headroom bars

Under each slider are that provider's usage windows, read by the quota broker from the sign-in the provider's own app is using (at most every 90 seconds, 180 for Antigravity, and without ever refreshing the sign-in). If a provider can't be read, the section shows a single bar, **Estimated**: local usage scaled from the last reading of the past 5 hours by the tokens Mast has recorded since; with no such reading it shows "Usage unavailable". Until the broker answers, or if the chosen device runs an older Mast, the bars show the same windows as the account rows. A window is a bar with a small tick at the provider's hard cap, how much is used, and how much room is left before the cap ("23% left to cap"). A bar turns amber when it is within 10 points of the cap, and red, reading "over cap by 4%", once it passes it. Windows the provider reports per model, such as a Sonnet or Opus limit, are listed under **Per model**. Dragging a slider moves the ticks at once, before you save. A provider with no usage reading shows "Usage unavailable".

The Mast menu bar's usage lines show the same cap: a tick in the bar at the cap's position, red once usage reaches it, and "cap 85%" after the percentage. The menu bar only shows it once it has the cap settings.

Nothing changes until you click **Save**. The device switcher at the top of the page picks whose settings you see and change; switching devices drops unsaved edits, while **Refresh** keeps them. Each device keeps one set of settings per Mast profile. The mode and caps are not set per project: you can't pick projects individually yet. Caps are per provider, not per account: two accounts of one provider share its hard cap, and the bars show the one in use.

A cap you don't adjust is saved exactly as it was. If something other than this page saved a fraction of a percent, or a hard cap outside 50% to 95%, the section shows that value, and you can change the mode without changing it.

Once connected, it will check each provider's usage every 90 seconds (180 seconds for Antigravity `/usage`) while orchestration is active. It will only read usage and never refresh a single-use sign-in token. If a provider can't be read, it will use an estimate from local token usage. A provider that needs sign-in, or has neither a reading nor an estimate, will get no new work.

- **Balanced failover** (default): new work will be spread by the room each provider has below the hard cap. A provider will keep the work until it reaches the soft cap and another provider below that cap has room. When no provider can take work, orchestration will pause and try again when one may have room.
- **Pinned freeze**: work will never move to another provider. When work would pass that provider's own hard cap, if you set one, or otherwise the freeze cap, or the provider's usage can't be trusted, Mast will freeze, save a checkpoint and notify you.

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
