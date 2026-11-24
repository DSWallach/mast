#!/bin/sh
# Fake zeron cursor shim for mast-harness tests: speaks the shim's JSONL
# protocol (see crates/harness/src/cursor/shim.mjs) without node or the SDK.
# Driven by crates/harness/tests/cursor.rs.

emit() { printf '%s\n' "$1"; }

# Models mode (argv, no stdin protocol): one catalog frame, real 1.0.28
# shapes — parameterized Auto + its bare `default` alias twin (skipped by the
# harness) + a plain model.
if [ "$1" = "models" ]; then
  emit '{"ev":"models","items":[{"id":"auto-smart","displayName":"Auto","parameters":[{"id":"optimize_for","displayName":"Optimize For","values":[{"value":"intelligence","displayName":"Intelligence"},{"value":"balanced","displayName":"Balance"},{"value":"cost","displayName":"Cost"}]}],"variants":[{"params":[{"id":"optimize_for","value":"balanced"}],"displayName":"Auto","isDefault":true}]},{"id":"default","displayName":"Auto","aliases":["auto"]},{"id":"claude-fable-5","displayName":"Claude Fable 5","description":"Anthropic frontier","parameters":[{"id":"thinking","values":[{"value":"enabled"},{"value":"disabled"}]}]}]}'
  exit 0
fi

read -r first || exit 1
case "$first" in
*'"op":"run"'*) ;;
*) emit '{"ev":"fatal","message":"expected op run first"}'; exit 1 ;;
esac

case "$first" in

*scenario:echo*)
  emit '{"ev":"ready","agentId":"agent-echo","model":"auto"}'
  case "$first" in
  *'"mcpServers":{"mast-agent":{'*'"command":"/opt/zeron/bin/zeron"'*) emit '{"ev":"text","text":"mcp:yes"}' ;;
  *) emit '{"ev":"text","text":"mcp:no"}' ;;
  esac
  case "$first" in
  *'RULES-MARK\n\n---\nThe instructions above apply'*) emit '{"ev":"text","text":"instructions:yes"}' ;;
  *) emit '{"ev":"text","text":"instructions:no"}' ;;
  esac
  # What the shim would restate after a summary, and whether the prompt has it.
  case "$first" in
  *'"repin":"RULES-MARK'*) emit '{"ev":"text","text":"repin:yes"}' ;;
  *) emit '{"ev":"text","text":"repin:no"}' ;;
  esac
  case "$first" in
  *'"instructionsInPrompt":true'*) emit '{"ev":"text","text":"inprompt:yes"}' ;;
  *) emit '{"ev":"text","text":"inprompt:no"}' ;;
  esac
  emit '{"ev":"turn","status":"finished"}'
  exit 0
  ;;

*scenario:first-turn-error*)
  emit '{"ev":"ready","agentId":"agent-first-error","model":"auto"}'
  emit '{"ev":"turn","status":"error","error":"network down"}'
  exit 0
  ;;

*scenario:usage-limit*)
  emit '{"ev":"ready","agentId":"agent-limit","model":"auto"}'
  emit '{"ev":"turn","status":"error","error":"You are out of usage. Upgrade your plan [code=PRO_USER_USAGE_LIMIT, requestId=req-1]"}'
  exit 0
  ;;

*scenario:rate-limit*)
  emit '{"ev":"fatal","message":"Rate limit exceeded: too many requests, quota used up [code=GENERIC_RATE_LIMIT_EXCEEDED, requestId=req-2]"}'
  exit 1
  ;;

*scenario:burst*)
  exec node "$(dirname "$0")/cursor-steering-peer.mjs" "$first"
  ;;

*scenario:followup-crash*)
  emit '{"ev":"ready","agentId":"agent-crash-followup","model":"composer-2.5"}'
  emit '{"ev":"turn","status":"finished"}'
  read -r next || exit 0
  echo "followup exploded" >&2
  exit 3
  ;;

*scenario:happy*)
  emit '{"ev":"ready","agentId":"agent-1","model":"composer-2.5"}'
  emit '{"ev":"thinking","text":"planning"}'
  emit '{"ev":"text","text":"Hello from cursor"}'
  emit '{"ev":"tool","phase":"start","id":"c1","name":"shell","args":{"command":"ls -la"}}'
  emit '{"ev":"tool","phase":"end","id":"c1","name":"shell","args":{"command":"ls -la"},"error":false}'
  # A spawned subagent: the task chip on the parent feed, its interior tagged.
  emit '{"ev":"tool","phase":"start","id":"task1","name":"task","args":{"description":"scan repo"}}'
  emit '{"ev":"text","text":"sub scanning","parent":"task1"}'
  emit '{"ev":"tool","phase":"start","id":"s1","name":"grep","args":{"pattern":"todo"},"parent":"task1"}'
  emit '{"ev":"tool","phase":"end","id":"s1","name":"grep","args":{"pattern":"todo"},"error":false,"parent":"task1"}'
  emit '{"ev":"tool","phase":"end","id":"task1","name":"task","args":{"description":"scan repo"},"error":false}'
  # Unknown frame kinds must be tolerated.
  emit '{"ev":"someNewThing","x":1}'
  emit '{"ev":"usage","input":11,"output":5}'
  emit '{"ev":"turn","status":"finished"}'
  # Parked: wait for a follow-up or stdin EOF.
  read -r next || exit 0
  case "$next" in
  *'"op":"user"'*)
    emit '{"ev":"text","text":"second turn"}'
    emit '{"ev":"turn","status":"finished"}'
    ;;
  esac
  exit 0
  ;;

*scenario:interrupt-fatal*)
  emit '{"ev":"ready","agentId":"agent-int-fatal","model":"auto"}'
  emit '{"ev":"text","text":"working"}'
  read -r msg || exit 0
  emit '{"ev":"fatal","message":"transport closed during cancellation"}'
  exit 1
  ;;

*scenario:interrupt*)
  emit '{"ev":"ready","agentId":"agent-int","model":"auto"}'
  emit '{"ev":"text","text":"working"}'
  read -r msg || exit 0
  case "$msg" in
  *'"op":"interrupt"'*)
    emit '{"ev":"turn","status":"cancelled"}'
    ;;
  esac
  exit 0
  ;;

*scenario:fatal*)
  emit '{"ev":"fatal","message":"Cursor SDK is not authenticated (its login is separate from `cursor-agent login`): set CURSOR_API_KEY from cursor.com/settings, then retry."}'
  exit 1
  ;;

*scenario:crash*)
  emit '{"ev":"ready","agentId":"agent-c","model":"auto"}'
  echo "shim exploded" >&2
  exit 3
  ;;

*)
  emit '{"ev":"fatal","message":"unknown scenario"}'
  exit 1
  ;;
esac
