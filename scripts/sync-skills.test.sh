#!/bin/sh
set -eu

script="$(cd "$(dirname "$0")" && pwd)/sync-skills.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
failures=0

fail() {
  echo "FAIL: $1" >&2
  failures=$((failures + 1))
}

mkdir -p "$tmp/stubs"
cat >"$tmp/stubs/ssh" <<'EOF'
#!/bin/sh
echo "ssh $*" >>"$CALLS"
shift
cd "$REMOTE_HOME" || exit 255
[ "$*" = "sh -s" ] && HOME="$REMOTE_HOME" exec sh -s
HOME="$REMOTE_HOME" exec sh -c "$*"
EOF
cat >"$tmp/stubs/rsync" <<'EOF'
#!/bin/sh
echo "rsync $*" >>"$CALLS"
EOF
chmod +x "$tmp/stubs/ssh" "$tmp/stubs/rsync"

setup() {
  rm -rf "$tmp/local" "$tmp/remote"
  mkdir -p "$tmp/local/.claude/rules" "$tmp/remote/Projects/perseu-stack/bin" \
    "$tmp/remote/.claude/skills" "$tmp/remote/.codex/skills"
  echo a >"$tmp/local/.claude/rules/a.md"
  echo b >"$tmp/local/.claude/rules/b.md"
  CALLS="$tmp/calls"
  : >"$CALLS"
  cat >"$tmp/remote/Projects/perseu-stack/bin/install-hooks" <<'EOF'
#!/bin/sh
echo "install-hooks $*" >>"$CALLS"
printf '%s' "$HOOKS_OUT"
exit "$HOOKS_EXIT"
EOF
  chmod +x "$tmp/remote/Projects/perseu-stack/bin/install-hooks"
}

run() {
  CALLS="$CALLS" REMOTE_HOME="$tmp/remote" HOME="$tmp/local" PATH="$tmp/stubs:$PATH" \
    HOOKS_OUT="$HOOKS_OUT" HOOKS_EXIT="$HOOKS_EXIT" sh "$script" "$@" >"$tmp/out" 2>&1
}

remote_tree() {
  (cd "$tmp/remote" && find . -print -type f -exec cksum {} + | LC_ALL=C sort)
}

setup
HOOKS_OUT='written  ~/.claude/settings.json
current  ~/.codex/hooks.json
linked   ~/.local/bin/heavy
' HOOKS_EXIT=0
if run testhost; then
  grep -qx 'hooks on testhost: 1 current, 1 linked, 1 written' "$tmp/out" \
    || fail "a sync does not print the install-hooks outcome in one line: $(cat "$tmp/out")"
  [ "$(grep -c '^install-hooks' "$CALLS")" = 1 ] || fail "a sync ran install-hooks $(grep -c '^install-hooks' "$CALLS") times"
  grep -qx 'install-hooks ' "$CALLS" || fail "a sync ran install-hooks with arguments: $(grep '^install-hooks' "$CALLS")"
  last_rsync=$(grep -n '^rsync' "$CALLS" | tail -1 | cut -d: -f1)
  hooks_at=$(grep -n '^install-hooks' "$CALLS" | cut -d: -f1)
  [ "${hooks_at:-0}" -gt "${last_rsync:-0}" ] || fail "install-hooks ran before the last mirror (line $hooks_at, last rsync $last_rsync)"
  ! grep -q '^ssh .*install-hooks' "$CALLS" || fail "a sync handed install-hooks to the remote login shell, not sh -s: $(grep '^ssh .*install-hooks' "$CALLS")"
else
  fail "a sync with a passing install-hooks exited non-zero: $(cat "$tmp/out")"
fi

setup
HOOKS_OUT='broken   ~/.claude/settings.json (Expecting value); fix it by hand, nothing was written
' HOOKS_EXIT=2
if run testhost; then
  fail "a sync whose install-hooks exits 2 exited 0"
else
  grep -q 'Expecting value' "$tmp/out" || fail "a failed sync does not show the remote's message: $(cat "$tmp/out")"
fi

for case in "0 current" "1 stale" "missing unknown"; do
  setup
  code=${case% *} want=${case#* }
  HOOKS_OUT='stale    ~/.claude/settings.json
' HOOKS_EXIT=$code
  [ "$code" = missing ] && rm "$tmp/remote/Projects/perseu-stack/bin/install-hooks"
  before=$(remote_tree)
  if run --check testhost; then
    grep -qx 'differ: 2' "$tmp/out" || fail "--check ($want) does not print differ: 2: $(cat "$tmp/out")"
    grep -qx "hooks: $want" "$tmp/out" || fail "--check does not print hooks: $want: $(cat "$tmp/out")"
    [ "$(wc -l <"$tmp/out")" -eq 2 ] || fail "--check ($want) printed $(wc -l <"$tmp/out") lines, not two: $(cat "$tmp/out")"
  else
    fail "--check ($want) exited non-zero: $(cat "$tmp/out")"
  fi
  ! grep -q '^rsync' "$CALLS" || fail "--check ($want) ran rsync"
  ! grep '^install-hooks' "$CALLS" | grep -qvx 'install-hooks --check' \
    || fail "--check ($want) ran install-hooks without --check: $(grep '^install-hooks' "$CALLS")"
  [ "$(remote_tree)" = "$before" ] || fail "--check ($want) changed the remote home"
  ! grep -q '^ssh .*install-hooks' "$CALLS" || fail "--check ($want) handed install-hooks to the remote login shell, not sh -s: $(grep '^ssh .*install-hooks' "$CALLS")"
done

setup
mkdir -p "$tmp/local/.gemini/config/skills/agy-skill"
echo s >"$tmp/local/.gemini/config/skills/agy-skill/SKILL.md"
echo g >"$tmp/local/.gemini/GEMINI.md"
HOOKS_OUT='' HOOKS_EXIT=0
if run testhost; then
  grep -q "^rsync .* $tmp/local/.gemini/config/skills/ testhost:.gemini/config/skills/$" "$CALLS" \
    || fail "a sync does not mirror ~/.gemini/config/skills: $(grep '^rsync' "$CALLS")"
  grep -q "^rsync .* $tmp/local/.gemini/GEMINI.md testhost:.gemini/GEMINI.md$" "$CALLS" \
    || fail "a sync does not mirror ~/.gemini/GEMINI.md: $(grep '^rsync' "$CALLS")"
else
  fail "a sync with gemini entries exited non-zero: $(cat "$tmp/out")"
fi

[ "$failures" -eq 0 ] || exit 1
echo "sync-skills.test.sh: ok"
