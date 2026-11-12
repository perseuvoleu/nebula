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
  for s in "/tmp/nvt-$$" "/tmp/nvt-$$-reuse" "/tmp/nvt-$$-rebuilt" "/tmp/nvt-$$-hung" "/tmp/nvt-$$-other" "/tmp/nvt-$$-frozen" "/tmp/nvt-$$-seed"; do
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

fixtures=$(mktemp -d)
fixture=$fixtures/nebula.db
perl -0777 -ne '/const MIGRATIONS: &\[&str\] = &\[(.*?)\n\];/s or die "no MIGRATIONS";
  my $block = $1; $block =~ s{^\s*//.*$}{}mg; my @m = $block =~ /"([^"]*)"/g;
  print "$_;\n" for @m; print "PRAGMA user_version = ", scalar(@m), ";\n"' \
  "$here/../../../crates/nebula-daemon/src/store.rs" | sqlite3 "$fixtures/schema.db"
cp "$fixtures/schema.db" "$fixture"
sqlite3 "$fixture" "PRAGMA journal_mode = WAL" >/dev/null
sqlite3 "$fixture" ".dbconfig no_ckpt_on_close on" "
  INSERT INTO projects (id, name, repo_path, created_at, workspace_id, host) VALUES
    ('p-alpha', 'alpha', '/nonexistent/real/alpha', 1, 'default', NULL),
    ('p-far', 'far', '/srv/far', 1, 'default', 'findl');
  INSERT INTO worktrees (id, project_id, path, branch, is_main, created_at) VALUES
    ('w-main', 'p-alpha', '/nonexistent/real/alpha', 'trunk', 1, 1),
    ('w-feat', 'p-alpha', '/nonexistent/real/alpha-feat', 'feature/seed', 0, 1);
  INSERT INTO agents (id, worktree_id, name, status, archived, claude_session_id, created_at) VALUES
    ('a-run', 'w-main', 'Runner', 'running', 0, 'sid-run', 1),
    ('a-ask', 'w-feat', 'Asker', 'needs_feedback', 0, 'sid-ask', 1),
    ('a-old', 'w-main', 'Shelved', 'finished', 1, 'sid-old', 1);
  INSERT INTO terminals (id, worktree_id, name, created_at, status) VALUES ('t-1', 'w-feat', 'shell', 1, 'running');
  INSERT INTO links (id, worktree_id, url, created_at) VALUES ('l-1', 'w-feat', 'https://example.invalid/pr?token=real', 1);" >/dev/null
cp "$fixture" "$fixtures/main-only.db"
check "the fixture's rows live only in its WAL" test "$(sqlite3 "$fixtures/main-only.db" "SELECT count(*) FROM agents")" = 0
fixture_sum=$(shasum -a 256 "$fixture")
export NV_STATE="/tmp/nvt-$$-seed"
seeded="$NV_STATE/data/nebula.db"
q() { sqlite3 -readonly "$seeded" "$1"; }
check "up --seed starts an instance from a copy of the db" "$vn" up --seed "$fixture"
real_state=$(cd "$NV_STATE" && pwd -P)
check "the seeded instance lists the copied sessions" \
  sh -c "'$vn' nebula agent list --all | grep -q Runner && '$vn' nebula agent list --all | grep -q Asker"
check "the seed leaves its source db byte-identical" test "$fixture_sum" = "$(shasum -a 256 "$fixture")"
check "remote projects are dropped" \
  test "$(q "SELECT group_concat(name, ' ') FROM (SELECT name FROM projects WHERE name != 'repo' ORDER BY name)")" = alpha
check "both worktrees point into the scratch repos" \
  test "$(q "SELECT count(*) FROM worktrees WHERE project_id = 'p-alpha' AND path LIKE '$real_state/repos/%'")" = 2
check "the project points at a scratch repo on its branch" \
  test "$(q "SELECT repo_path FROM projects WHERE id = 'p-alpha'")" = "$real_state/repos/alpha"
check "the scratch repo is on the primary's branch" test "$(git -C "$real_state/repos/alpha" branch --show-current)" = trunk
feat_path=$(q "SELECT path FROM worktrees WHERE id = 'w-feat'")
check "the linked worktree is a scratch checkout of its branch" \
  sh -c "case '$feat_path' in '$real_state/repos/'*) test \"\$(git -C '$feat_path' branch --show-current)\" = feature/seed ;; *) false ;; esac"
check "sessions keep name and archived flag, lose resume ids and live statuses" \
  test "$(q "SELECT group_concat(name || ' ' || status || ' ' || archived || ' ' || ifnull(claude_session_id, '-'), ', ') FROM (SELECT * FROM agents ORDER BY name)")" \
  = "Asker disconnected 0 -, Runner disconnected 0 -, Shelved finished 1 -"
check "copied links are dropped" test "$(q "SELECT count(*) FROM links")" = 0
check "down removes the seeded instance" "$vn" down

seed_refused() {
  ! "$vn" up --seed "$1" >/dev/null 2>&1 && test ! -e "$NV_STATE"
}
cp "$fixtures/schema.db" "$fixtures/orphan.db"
sqlite3 "$fixtures/orphan.db" "
  INSERT INTO projects (id, name, repo_path, created_at, workspace_id) VALUES ('p-alpha', 'alpha', '/nonexistent/real/alpha', 1, 'default');
  INSERT INTO worktrees (id, project_id, path, branch, is_main, created_at) VALUES
    ('w-main', 'p-alpha', '/nonexistent/real/alpha', 'trunk', 1, 1),
    ('w-orphan', 'p-gone', '/nonexistent/real/orphan', 'lost', 0, 1);"
check "a worktree left at a real path refuses the seed" seed_refused "$fixtures/orphan.db"
cp "$fixtures/schema.db" "$fixtures/newline.db"
sqlite3 "$fixtures/newline.db" "
  INSERT INTO projects (id, name, repo_path, created_at, workspace_id) VALUES ('p-nl', 'nl', '/nonexistent/real' || char(10) || 'project', 1, 'default');
  INSERT INTO worktrees (id, project_id, path, branch, is_main, created_at) VALUES
    ('w-nl', 'p-nl', '/nonexistent/real' || char(10) || 'project', 'trunk', 1, 1);"
caller=$fixtures/caller
git init -q "$caller" && git -C "$caller" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
check "a path with a newline refuses the seed" eval '(cd "$caller" && seed_refused "$fixtures/newline.db")'
check "a refused seed adds no worktree to the caller's repo" test "$(git -C "$caller" worktree list | wc -l | tr -d ' ')" = 1
mkdir "$fixtures/bin"
printf '#!/bin/sh\ncase "$*" in *"GROUP BY"*) echo "Error: disk I/O error" >&2; exit 10 ;; esac\nexec %s "$@"\n' "$(command -v sqlite3)" >"$fixtures/bin/sqlite3"
chmod +x "$fixtures/bin/sqlite3"
check "a failed enumeration refuses the seed" eval '(PATH="$fixtures/bin:$PATH" && seed_refused "$fixture")'
rm -rf "$fixtures"

echo "$failures failure(s)"
[ "$failures" -eq 0 ]
