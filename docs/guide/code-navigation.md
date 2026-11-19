# Navigate code

Open a file with `⌘O`, or search symbol names with `⌥⇧⌘O`. Symbol results show their kind, file and line. Press Enter to open a result or `⌥Enter` to attach its current source to the chat. Search is name-based: several results may have the same name.

In a code file, use the file-symbols button to open an outline, then select a row to jump. `⌘F` opens find; use the case button and the arrows or `⌘G` / `⇧⌘G` to move through matches. Very common queries show the first 10,000 matches. `⌃-` and `⌃⇧-` move back and forward through file locations.

The history button lists the latest 100 commits touching the file, following renames. Select a commit to show that file's diff. History uses local Git data and does not fetch from a remote.

## See who and which chat wrote a line

Turn on the blame gutter with the branch button in the file toolbar, `⌥⌘G`, or View → Toggle Blame Gutter. It is off by default, and nothing is read until you turn it on. Each run of lines shows the author and how long ago, plus a small chip with the Mast chat behind the commit when Mast knows it. Uncommitted lines read "Not committed". The shortcut can be rebound in Settings.

Hover a run for the commit (subject, short hash, author, date) and the chat. Click the commit to open that commit's diff for the file in History. Click the chat to open it, scrolled to the turn that edited the file when the transcript has one.

Where the chat comes from:

- **Recorded.** When a worktree a chat has bound (`bind_chat_worktree` or the Changes picker) gets new commits, Mast stores commit, chat and turn range in your profile, but only for a commit that is on that worktree's own first-parent line, was not already on a remote-tracking ref or another local branch before the move (pulls, merges of main and rebases onto main bring nothing in; a push or new branch made right after the commit does not hide it), was committed while exactly one bound chat was Working or within 15 seconds after its turn ended (a chat waiting on a question, or two chats working at once, attributes nothing), is not a rewrite of commits that already existed, and is not a merge commit. The first record of a commit wins; it is never reassigned. Nothing is added to the repository: no hooks, trailers or config. If a recorded commit is rewritten by a rebase or amend, Mast matches it to the old one by `git patch-id` and copies the record to the new commit. At most 200 commits are walked per move and 5,000 records are kept per repository.
- **Probably.** With no record, a commit that is in `default-branch..branch` of a branch a chat in the same repository is bound to is shown as "probably from" that chat (chip prefixed with `~`). The default branch's own history is never guessed.
- A squash made by the agent while it is Working is attributed to that chat; the squashed commit is new work with no record to inherit. A squash you make later by hand is a rewrite of commits that had records only if the patch matches exactly, so it usually loses the attribution.
- Commits made before Mast was watching, outside a bound worktree, or while no chat was working have no chat. Turn links need the chat's transcript on this device.

Blame uses local Git data, is computed once per file and HEAD, and skips files over 512 KiB. The gutter follows commits and checkouts; with unsaved edits it greys out and says "Save to update blame".

Symbols use the bundled tree-sitter tags queries for Rust, JavaScript, TypeScript, Python, Go, C, C++, C#, Java, Swift, Ruby, PHP and Lua. JSON, JSONC, Bash, TOML, Markdown, HTML, CSS, YAML, Kotlin, SQL, Nix, Make and Dockerfile do not currently have usable bundled tags queries. Files over 1 MiB and files with lines over 20,000 bytes are skipped. The index covers up to 50,000 supported files and 200,000 symbols, subject to a memory limit; the picker warns when it reaches a limit. Opening the picker checks a sample of up to 128 recently modified files in the background; Refresh checks and re-indexes the entire file list. Existing file index repair signals also update symbols. Search does not use a language server or infer types.
