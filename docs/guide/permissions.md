# Permission rules

The approval chip under the composer has three modes:

- **Ask**: the agent asks before risky actions, as it always has.
- **Rules**: requests that your rules allow run without asking, requests your
  rules deny are refused, and everything else asks you.
- **Auto**: the agent never asks.

Rules mode is the default for new and imported scheduled tasks.

## The rules file

Rules live in `~/.mast/settings.json`. The format is the `permissions` block of
Claude Code's `settings.json`, so you can copy your Claude rules across:

```json
{
  "permissions": {
    "allow": [
      "Bash(/Users/me/bin/gh_read:*)",
      "Bash(git log:*)",
      "mcp__claude_ai_Notion__notion-update-page"
    ],
    "deny": ["Bash(git push:*)"],
    "ask": []
  }
}
```

Mast reads the file again whenever it changes. A file that isn't valid JSON is
reported in **Settings → Agents → Permission rules**, and every request asks
until you fix it.

A deny rule beats an ask rule, and an ask rule beats an allow rule.

| Rule | Matches |
|---|---|
| `Bash(git log:*)` | Commands that start with these words |
| `Bash(npm test)` | Exactly this command |
| `Bash(git diff * main)` | `*` matches any text |
| `mcp__notion__notion-fetch` | One MCP tool |
| `mcp__notion` or `mcp__notion__*` | Every tool on that server |
| `WebSearch` | A tool by name |

Mast splits a command at `&&`, `||`, `;`, `|` and new lines, and every part needs
its own allow rule. A command Mast can't read exactly, such as one with `$(…)`,
backquotes, redirects or variables, always asks. Commands that use `sudo` or a
recursive `rm`, and anything that names `~/.mast/settings.json`, always ask, whatever
the rules say. A command that contains a deny or ask rule's words with options
in between, like `git -C repo push` against `Bash(git push:*)`, asks too.

Rules with arguments other than `Bash(…)`, such as `Edit(src/**)` or
`WebFetch(domain:…)`, aren't checked by Mast. The settings page lists them. If
one is a deny or ask rule, every request for that tool asks, so the rule is
never skipped.

A chat in Rules mode can't start a child chat that's laxer than Rules: the
child gets Rules unless it's Auto, and only an Auto parent can start an Auto child.

### MCP server names

The same server can have a different name in each agent. Mast treats
`claude_ai_Notion`, `plugin_engineering_notion` and `notion` as the same server.
For a name it can't match, such as a connector ID, add an alias:

```json
{ "mcpServerAliases": { "82b0abf1-2c4c-44f2-8e55-b5ebc287f415": "notion" } }
```

## Always allow

In Rules mode, a prompt that Mast can turn into a rule offers **Always allow:
`<rule>`** next to Yes and No. Choosing it adds the rule to
`permissions.allow` and allows the request. If the file can't be written, the
request stays open and you see the error. Only you can choose Always allow;
agents answering through `respond_to_input` can't.

The suggested rule covers the program and its first subcommand, for example
`Bash(git log:*)`. When the second word is an option, it covers only that exact
command. Shells, interpreters and wrappers like `env` or `xargs` get no
suggestion. To allow something wider or narrower, edit the file. Remove
rules from **Settings → Agents → Permission rules** or from the file.

## Providers

| Agent | What Rules mode checks |
|---|---|
| Claude | Every tool call Claude's own settings don't already allow: commands, MCP tools, other tools. Claude's own allow list still applies inside Claude. |
| Codex | MCP tool calls, and commands that ask to leave the sandbox (network access, for example). Commands inside the workspace sandbox run without asking, as in Ask mode. |
| ACP agents | Commands sent with their full text in `rawInput.command`, and MCP tools named in Gemini CLI's `_meta["gemini-cli"].mcp`. Mast answers with the agent's allow-once option, so no grant is stored in the agent; if the agent offers no allow-once option, the request is cancelled. Mast also stops choosing the agent's no-prompt mode. |
| OpenCode | Shell commands (OpenCode 1.x, which sends the full command), and MCP tools with an exact `mcp__server__tool` rule. OpenCode 2.x shell requests always ask, because they don't include the full command. |
| Cursor | Not supported: Cursor doesn't send permission requests to Mast. |

When the chat belongs to a Space, every request Mast allows or denies by rule is
recorded as a Permission friction report naming the rule, so you can review what
an unattended run did.
