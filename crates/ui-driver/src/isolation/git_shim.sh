#!/bin/sh
# Logging git shim. It is not a confinement layer: the outer sandbox-exec write
# rule is. It records each spawn, drops the caller's git configuration and
# hooks, then execs the real git.
printf '%s\n' "git $*" >> "@GIT_LOG@"
for name in $(env | sed -n 's/^\(GIT_[A-Z_]*\)=.*/\1/p'); do
  case "$name" in
    GIT_AUTHOR_DATE|GIT_COMMITTER_DATE|GIT_AUTHOR_NAME|GIT_AUTHOR_EMAIL|GIT_COMMITTER_NAME|GIT_COMMITTER_EMAIL) ;;
    *) unset "$name" ;;
  esac
done
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_OPTIONAL_LOCKS=0
export GIT_AUTHOR_NAME="${GIT_AUTHOR_NAME:-Driver fixture}" GIT_AUTHOR_EMAIL="${GIT_AUTHOR_EMAIL:-driver@example.invalid}"
export GIT_COMMITTER_NAME="${GIT_COMMITTER_NAME:-Driver fixture}" GIT_COMMITTER_EMAIL="${GIT_COMMITTER_EMAIL:-driver@example.invalid}"
export GIT_CONFIG_COUNT=2 GIT_CONFIG_KEY_0=core.hooksPath GIT_CONFIG_VALUE_0=/dev/null GIT_CONFIG_KEY_1=core.fsmonitor GIT_CONFIG_VALUE_1=false
exec /usr/bin/git "$@"
