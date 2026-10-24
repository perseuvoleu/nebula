#!/usr/bin/env bash
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd -P)
vn="$here/vn"
export NV_STATE="/tmp/nvt-$$"
export NV_EVIDENCE="$(mktemp -d)/evidence"
mkdir -p "$NV_EVIDENCE"
failures=0
check() {
  local what=$1
  shift
  if "$@"; then echo "ok   $what"; else echo "FAIL $what"; failures=$((failures + 1)); fi
}
cleanup() {
  for s in "/tmp/nvt-$$" "/tmp/nvt-$$-reuse" "/tmp/nvt-$$-rebuilt" "/tmp/nvt-$$-hung" "/tmp/nvt-$$-other" "/tmp/nvt-$$-frozen"; do
    NV_STATE=$s "$vn" down >/dev/null 2>&1
  done
}
trap cleanup EXIT

user_pid=$(cat "/tmp/nebula-$(id -u)/daemon.pid" 2>/dev/null || true)

check "up starts an instance" "$vn" up
first_pid=$(cat "$NV_STATE/daemon.pid")
check "a second up keeps the same daemon" "$vn" up
check "the second up left the pid alone" test "$first_pid" = "$(cat "$NV_STATE/daemon.pid")"
check "doctor passes on a fresh instance" "$vn" doctor
check "the daemon socket lives in the state dir" test -S "$NV_STATE/rt/daemon.sock"

check "a lead session spawns" "$vn" nebula agent new --project repo --worktree main --name Lead
check "a codex session spawns" "$vn" nebula agent new --project repo --worktree main --kind codex --name Coder
check "codex hooks land in the scratch CODEX_HOME" test -s "$NV_STATE/codex/hooks.json"
NEBULA_AGENT_ID=inherited-from-caller "$vn" nebula agent new --project repo --worktree main --name Loner >/dev/null
check "a caller's session identity does not leak into the instance" \
  sh -c "'$vn' nebula agent show Loner --project repo | grep -q '\"spawned_by\":null'"

check "tui opens on the project" "$vn" tui
check "the sidebar shows the lead" "$vn" wait Lead 20
check "shot writes the screen to evidence" sh -c "'$vn' shot sidebar >/dev/null && grep -q Lead '$NV_EVIDENCE/sidebar.txt'"

agent_pids=$(pgrep -P "$first_pid" | tr '\n' ' ')
check "down succeeds" "$vn" down
check "the daemon is gone" sh -c "! kill -0 $first_pid 2>/dev/null"
for p in $agent_pids; do check "agent pty $p is gone" sh -c "! kill -0 $p 2>/dev/null"; done
check "the tmux server is gone" sh -c "! tmux -S '$NV_STATE/tmux.sock' has-session 2>/dev/null"
check "the state dir is removed" test ! -e "$NV_STATE"
check "evidence survives down" test -s "$NV_EVIDENCE/sidebar.txt"
check "doctor reports the daemon down" sh -c "! '$vn' doctor >'$NV_EVIDENCE/doctor-down.txt' 2>&1 && grep -q 'FAIL daemon' '$NV_EVIDENCE/doctor-down.txt'"
check "a second down is a no-op" "$vn" down
if [ -n "$user_pid" ]; then
  check "the user's daemon still runs" kill -0 "$user_pid"
fi

mkdir -p "$NV_STATE.real" && ln -s "$NV_STATE.real" "$NV_STATE"
check "up refuses a linked state dir" sh -c "! '$vn' up >'$NV_EVIDENCE/up-link.txt' 2>&1 && grep -q 'is a link' '$NV_EVIDENCE/up-link.txt'"
rm -f "$NV_STATE" && rmdir "$NV_STATE.real"

unowned="/tmp/nvt-$$-unowned"
mkdir -p "$unowned" && touch "$unowned/sentinel"
check "down refuses a state dir vn did not make" \
  sh -c "! NV_STATE='$unowned' '$vn' down >'$NV_EVIDENCE/down-unowned.txt' 2>&1 && grep -qF '$unowned' '$NV_EVIDENCE/down-unowned.txt'"
check "up refuses a state dir vn did not make" \
  sh -c "! NV_STATE='$unowned' '$vn' up >'$NV_EVIDENCE/up-unowned.txt' 2>&1 && grep -qF '$unowned' '$NV_EVIDENCE/up-unowned.txt'"
check "the unowned dir keeps its files" test -e "$unowned/sentinel"
rm -rf "$unowned"

crash_daemon() {
  local pid
  pid=$(cat "$NV_STATE/daemon.pid")
  pkill -KILL -P "$pid"
  kill -KILL "$pid"
}
export NV_STATE="/tmp/nvt-$$-reuse"
sleep 60 &
stranger=$!
"$vn" up >/dev/null
crash_daemon
echo "$stranger" >"$NV_STATE/daemon.pid"
check "up does not take a reused pid for its daemon" sh -c "'$vn' up | grep -q 'vn: up at'"
crash_daemon
echo "$stranger" >"$NV_STATE/daemon.pid"
check "down on a reused pid succeeds" "$vn" down
check "down leaves the process that reused the pid alone" kill -0 "$stranger"
kill "$stranger"
NV_STATE="/tmp/nvt-$$-other" "$vn" up >/dev/null
other_pid=$(cat "/tmp/nvt-$$-other/daemon.pid")
"$vn" up >/dev/null
crash_daemon
echo "$other_pid" >"$NV_STATE/daemon.pid"
check "down on a pid owned by another instance succeeds" "$vn" down
check "down leaves another instance's daemon running" kill -0 "$other_pid"
NV_STATE="/tmp/nvt-$$-other" "$vn" down >/dev/null

bindir=$(mktemp -d)
cp -c "$here/../../../target/verify-nebula/debug/nebula" "$bindir/nebula"
export NV_STATE="/tmp/nvt-$$-rebuilt" NV_BIN="$bindir/nebula"
"$vn" up >/dev/null
old_pid=$(cat "$NV_STATE/daemon.pid")
cp -c "$NV_BIN" "$NV_BIN.new" && printf x >>"$NV_BIN.new" && mv "$NV_BIN.new" "$NV_BIN"
check "doctor flags a binary rebuilt since up" \
  sh -c "! '$vn' doctor >'$NV_EVIDENCE/doctor-rebuilt.txt' 2>&1 && grep -q 'FAIL daemon binary' '$NV_EVIDENCE/doctor-rebuilt.txt'"
check "up restarts a daemon started from an older binary" "$vn" up
check "the daemon from the older binary is gone" sh -c "! kill -0 $old_pid 2>/dev/null"
check "doctor passes after the restart" "$vn" doctor
"$vn" down >/dev/null
rm -rf "$bindir"

cat >"$bindir.frozen" <<EOF
#!/bin/sh
[ "\$1" = agent ] && kill -STOP "\$(cat "\$NEBULA_RUNTIME_DIR/../daemon.pid")"
exec "$here/../../../target/verify-nebula/debug/nebula" "\$@"
EOF
chmod +x "$bindir.frozen"
export NV_STATE="/tmp/nvt-$$-frozen" NV_BIN="$bindir.frozen"
"$vn" up >/dev/null 2>&1 &
up_pid=$!
for _ in $(seq 50); do [ -s "$NV_STATE/daemon.pid" ] && break; sleep 0.1; done
frozen_pid=$(cat "$NV_STATE/daemon.pid")
for _ in $(seq 250); do kill -0 "$up_pid" 2>/dev/null || break; sleep 0.1; done
check "up gives up on a daemon that stops answering within 25s" sh -c "! kill -0 $up_pid 2>/dev/null"
check "up stops the daemon that stopped answering" sh -c "! kill -0 $frozen_pid 2>/dev/null"
check "up removes the state dir of the daemon that stopped answering" test ! -e "$NV_STATE"
pkill -KILL -P "$up_pid" 2>/dev/null
kill -KILL "$up_pid" "$frozen_pid" 2>/dev/null
rm -f "$bindir.frozen"

cat >"$bindir.nebula" <<'EOF'
#!/bin/sh
[ "$1" = daemon ] || exit 1
while :; do sleep 1; done
EOF
chmod +x "$bindir.nebula"
export NV_STATE="/tmp/nvt-$$-hung" NV_BIN="$bindir.nebula"
check "up fails when the daemon never answers" sh -c "! '$vn' up >/dev/null 2>&1"
check "a failed up stops the daemon it started" sh -c "! pgrep -f '$NV_BIN' >/dev/null"
check "a failed up removes its state dir" test ! -e "$NV_STATE"
"$vn" up >/dev/null 2>&1 &
up_pid=$!
for _ in $(seq 50); do pgrep -f "$NV_BIN" >/dev/null && break; sleep 0.1; done
kill -TERM "$up_pid"
wait "$up_pid"
check "an interrupted up stops the daemon it started" sh -c "! pgrep -f '$NV_BIN' >/dev/null"
check "an interrupted up removes its state dir" test ! -e "$NV_STATE"
pkill -f "$NV_BIN"
rm -f "$NV_BIN"
unset NV_BIN

echo "$failures failure(s)"
[ "$failures" -eq 0 ]
