# Navigate code

Open a file with `⌘O`, or search symbol names with `⌥⇧⌘O`. Symbol results show their kind, file and line. Press Enter to open a result or `⌥Enter` to attach its current source to the chat. Search is name-based: several results may have the same name.

In a code file, use the file-symbols button to open an outline, then select a row to jump. `⌘F` opens find; use the case button and the arrows or `⌘G` / `⇧⌘G` to move through matches. Very common queries show the first 10,000 matches. `⌃-` and `⌃⇧-` move back and forward through file locations.

The history button lists the latest 100 commits touching the file, following renames. Select a commit to show that file's diff. History uses local Git data and does not fetch from a remote.

Symbols use the bundled tree-sitter tags queries for Rust, JavaScript, TypeScript, Python, Go, C, C++, C#, Java, Swift, Ruby, PHP and Lua. JSON, JSONC, Bash, TOML, Markdown, HTML, CSS, YAML, Kotlin, SQL, Nix, Make and Dockerfile do not currently have usable bundled tags queries. Files over 1 MiB and files with lines over 20,000 bytes are skipped. The index covers up to 50,000 supported files and 200,000 symbols, subject to a memory limit; the picker warns when it reaches a limit. Opening the picker checks a sample of up to 128 recently modified files in the background; Refresh checks and re-indexes the entire file list. Existing file index repair signals also update symbols. Search does not use a language server or infer types.
