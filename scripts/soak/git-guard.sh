#!/bin/sh
# Read-only git guard for perf-soak (shim placed first on PATH).
# Logs every attempt to $MAST_SOAK_GIT_LOG ("A <subcmd>" allowed, "R <subcmd>" refused),
# refuses anything that can write, then execs the real git with optional locks off.
sub=""; skip=0; rest=""
for a in "$@"; do
  if [ "$skip" = 1 ]; then skip=0; continue; fi
  if [ -z "$sub" ]; then
    case "$a" in
      -C|-c|--git-dir|--work-tree|--namespace|--exec-path) skip=1 ;;
      -*) ;;
      *) sub="$a" ;;
    esac
  else
    rest="$rest $a"
  fi
done
refuse() {
  printf 'R %s\n' "$sub" >> "${MAST_SOAK_GIT_LOG:-/dev/null}"
  echo "soak-git: refused write-capable command: git $*" >&2
  exit 128
}
has() { for w in $rest; do case "$w" in "$1") return 0 ;; esac; done; return 1; }
case "$sub" in
  status|rev-parse|diff|diff-index|diff-files|diff-tree|log|show|ls-files|ls-tree|cat-file|for-each-ref|show-ref|rev-list|merge-base|describe|name-rev|check-ignore|blame|var|version|--version|help|"") ;;
  worktree) has list || refuse "$@" ;;
  branch) for w in $rest; do case "$w" in -d|-D|-m|-M|-c|-C|-f|--delete|--move|--copy|--force|--set-upstream-to*|--unset-upstream|-u|--edit-description) refuse "$@" ;; esac; done ;;
  symbolic-ref) for w in $rest; do case "$w" in -d|--delete) refuse "$@" ;; esac; done
                n=0; for w in $rest; do case "$w" in -*) ;; *) n=$((n+1)) ;; esac; done
                [ "$n" -le 1 ] || refuse "$@" ;;
  config) has --get || has --get-all || has --get-regexp || has --list || has -l || has --get-urlmatch || refuse "$@" ;;
  remote) set -- $rest; case "${1:-}" in ""|-v|--verbose|get-url) ;; *) refuse "git remote $rest" ;; esac ;;
  stash) has list || refuse "$@" ;;
  hash-object) has -w && refuse "$@" ;;
  *) refuse "$@" ;;
esac
printf 'A %s\n' "$sub" >> "${MAST_SOAK_GIT_LOG:-/dev/null}"
# Per-spawn detail (cwd and full args) next to the log, for tallying callers.
[ -n "${MAST_SOAK_GIT_LOG:-}" ] && printf '%s\t%s\t%s\n' "$(date +%s)" "$PWD" "$*" >> "$MAST_SOAK_GIT_LOG.detail"
GIT_OPTIONAL_LOCKS=0 exec "${MAST_SOAK_REAL_GIT:?}" "$@"
