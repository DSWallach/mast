#!/usr/bin/python3
"""Conservative git shim for driver fixtures and read-only inspection."""
import os
import sys


def contained(path, root):
    return os.path.commonpath((path, root)) == root


def main():
    scratch = os.path.realpath(os.environ["MAST_DRIVER_ROOT"])
    root = os.path.realpath(os.path.join(scratch, "repos"))
    log = os.path.join(scratch, "git.log")
    if root != os.path.join(scratch, "repos") or not os.path.isdir(root):
        print("driver git: invalid repository root", file=sys.stderr)
        return 128
    cwd = os.path.realpath(os.getcwd())
    args = sys.argv[1:]
    while len(args) >= 2 and args[0] == "-C":
        cwd = os.path.realpath(os.path.join(cwd, args[1]))
        args = args[2:]
    allowed = contained(cwd, root) and bool(args)
    subcommand = args[0] if args else ""
    # Git's .git indirection (including linked worktrees) must remain inside
    # the fixture root; otherwise even a harmless-looking command could read a
    # repository outside it.
    candidate = cwd
    git_entry = None
    while contained(candidate, root):
        entry = os.path.join(candidate, ".git")
        if os.path.lexists(entry):
            git_entry = entry
            break
        if candidate == root:
            break
        candidate = os.path.dirname(candidate)
    if subcommand == "init":
        allowed = allowed and git_entry is None
    elif git_entry is None:
        allowed = False
    elif not contained(os.path.realpath(git_entry), root):
        allowed = False
    else:
        if os.path.isfile(git_entry) and not os.path.isdir(git_entry):
            try:
                with open(git_entry, encoding="utf-8") as entry:
                    text = entry.read(4096).strip()
                git_dir = os.path.realpath(os.path.join(candidate, text[8:])) if text.startswith("gitdir: ") else ""
            except (OSError, UnicodeError):
                git_dir = ""
        else:
            git_dir = os.path.realpath(git_entry)
        allowed = allowed and bool(git_dir) and contained(git_dir, root) and os.path.isdir(git_dir)
        common_dir = git_dir
        common_file = os.path.join(git_dir, "commondir")
        if allowed and os.path.isfile(common_file):
            if not contained(os.path.realpath(common_file), root):
                print("driver git: invalid common directory", file=sys.stderr)
                return 128
            with open(common_file, encoding="utf-8") as entry:
                common_dir = os.path.realpath(os.path.join(git_dir, entry.read(4096).strip()))
            allowed = contained(common_dir, root) and os.path.isdir(common_dir)
        if allowed:
            # Git metadata can redirect object reads and config includes outside
            # the fixture. Refuse links and alternates, and only accept the
            # simple core config produced by the fixture's git init.
            count = 0
            for directory, names, files in os.walk(common_dir, followlinks=False):
                count += len(names) + len(files)
                if count > 10000 or any(os.path.islink(os.path.join(directory, name)) for name in names + files):
                    allowed = False
                    break
            worktrees_dir = os.path.join(common_dir, "worktrees")
            if allowed and os.path.isdir(worktrees_dir):
                for name in os.listdir(worktrees_dir):
                    pointer = os.path.join(worktrees_dir, name, "gitdir")
                    try:
                        with open(pointer, encoding="utf-8") as entry:
                            target = os.path.realpath(entry.read(4096).strip())
                        if not contained(target, root):
                            allowed = False
                            break
                    except (OSError, UnicodeError):
                        allowed = False
                        break
            if os.path.lexists(os.path.join(common_dir, "objects", "info", "alternates")):
                allowed = False
            if os.path.lexists(os.path.join(git_dir, "config.worktree")):
                allowed = False
            config = os.path.join(common_dir, "config")
            try:
                with open(config, encoding="utf-8") as entry:
                    lines = entry.read(16385).splitlines()
                if sum(len(line) for line in lines) > 16384:
                    allowed = False
                keys = {"repositoryformatversion", "filemode", "bare", "logallrefupdates", "ignorecase", "precomposeunicode"}
                for line in lines:
                    line = line.strip()
                    if not line or line.startswith(("#", ";")) or line.lower() == "[core]":
                        continue
                    key, separator, value = line.partition("=")
                    if not separator or key.strip().lower() not in keys or value.strip().lower() not in ("0", "true", "false"):
                        allowed = False
                        break
            except (OSError, UnicodeError):
                allowed = False
    rest = args[1:]
    # Only commands and options with explicitly understood effects are allowed.
    # The fixture creates an empty commit and detached linked worktrees.
    if subcommand == "init":
        allowed = allowed and rest == ["-q", "-b", "main"]
    elif subcommand == "commit":
        allowed = allowed and rest == ["-q", "--allow-empty", "-m", "initial"]
    elif subcommand == "worktree":
        if rest == ["list", "--porcelain"]:
            pass
        elif len(rest) == 4 and rest[:3] == ["add", "-q", "--detach"]:
            target = os.path.abspath(os.path.join(cwd, rest[3]))
            parent = os.path.realpath(os.path.dirname(target))
            allowed = allowed and contained(parent, root) and not os.path.lexists(target)
        else:
            allowed = False
    elif subcommand in ("status", "rev-parse", "diff", "diff-index", "diff-files", "diff-tree", "log", "show", "ls-files", "ls-tree", "cat-file", "for-each-ref", "show-ref", "rev-list", "merge-base", "describe", "name-rev", "check-ignore", "blame", "branch"):
        # Reject path redirection, configuration, external execution and option
        # forms we do not recognize. Positional arguments are deliberately
        # limited to revisions; paths can be added after an explicit review.
        safe_options = {"--porcelain", "--porcelain=v1", "--porcelain=v2", "--branch", "-b", "--short", "--show-toplevel", "--git-dir", "--is-inside-work-tree", "--abbrev-ref", "--verify", "--quiet", "-q", "--no-color", "--no-ext-diff", "--name-only", "--name-status", "--stat", "--cached", "--staged", "--untracked-files=no", "--all", "--list", "-a", "-r", "--contains", "--merged", "--no-merged", "--count", "--oneline", "--no-pager", "--format=%(refname:short)"}
        allowed = allowed and all(
            arg in safe_options or (not arg.startswith("-") and "/" not in arg and ".." not in arg and "\\\\" not in arg and ":" not in arg)
            for arg in rest
        )
    else:
        allowed = False
    with open(log, "a", encoding="utf-8") as output:
        output.write(("A " if allowed else "R ") + subcommand.replace("\n", "?") + "\n")
    if not allowed:
        print("driver git: refused command", file=sys.stderr)
        return 128
    os.chdir(cwd)
    for name in list(os.environ):
        if name.startswith("GIT_"):
            del os.environ[name]
    os.environ["GIT_AUTHOR_NAME"] = "Driver fixture"
    os.environ["GIT_AUTHOR_EMAIL"] = "driver@example.invalid"
    os.environ["GIT_COMMITTER_NAME"] = "Driver fixture"
    os.environ["GIT_COMMITTER_EMAIL"] = "driver@example.invalid"
    os.environ["GIT_AUTHOR_DATE"] = "2000-01-01T00:00:00+0000"
    os.environ["GIT_COMMITTER_DATE"] = "2000-01-01T00:00:00+0000"
    os.environ["GIT_CONFIG_NOSYSTEM"] = "1"
    os.environ["GIT_CONFIG_GLOBAL"] = "/dev/null"
    os.environ["GIT_CONFIG_COUNT"] = "3"
    for index, (key, value) in enumerate((("core.hooksPath", "/dev/null"), ("core.fsmonitor", "false"), ("diff.external", ""))):
        os.environ[f"GIT_CONFIG_KEY_{index}"] = key
        os.environ[f"GIT_CONFIG_VALUE_{index}"] = value
    os.environ["GIT_OPTIONAL_LOCKS"] = "0"
    os.execv("/usr/bin/git", ["git", *args])


if __name__ == "__main__":
    sys.exit(main())
