#!/usr/bin/env bash
# release.json is written only after both suites pass, so a release without it is rebuilt.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
C=$(git -C "$ROOT" rev-parse HEAD)
[ -z "$(git -C "$ROOT" status --porcelain)" ] || { echo "$ROOT has uncommitted changes; build from a clean checkout" >&2; exit 1; }
RELEASES=${NEBULA_WEB_RELEASES:-$HOME/.local/share/nebula/web-releases}
VENV=${NEBULA_WEB_VENV:-$HOME/.local/share/nebula/web-venv}
REL="$RELEASES/$C"
LOG="${NEBULA_WEB_BUILD_LOGS:-$HOME/.cache/nebula-web-build}/$C"
run() { if command -v heavy >/dev/null; then heavy "$@"; else "$@"; fi; }
mkdir -p "$LOG" "$RELEASES"
chmod 700 "$RELEASES"

if ! command -v pnpm >/dev/null; then
  npm install --silent --prefix "$HOME/.local/share/pnpm-cli" pnpm@10.14.0 >"$LOG/pnpm-install.log" 2>&1
  mkdir -p "$HOME/.local/bin"
  ln -sf "$HOME/.local/share/pnpm-cli/node_modules/.bin/pnpm" "$HOME/.local/bin/pnpm"
  export PATH="$HOME/.local/bin:$PATH"
fi
echo "pnpm $(pnpm --version)"

if [ ! -x "$VENV/bin/python" ]; then
  python3 -m venv --without-pip "$VENV"
  curl -fsS https://bootstrap.pypa.io/get-pip.py -o "$LOG/get-pip.py"
  "$VENV/bin/python" "$LOG/get-pip.py" -q >"$LOG/get-pip.log" 2>&1
fi

if [ -e "$REL/release.json" ]; then echo "release exists: $REL"; else
  if [ -e "$REL" ]; then chmod -R u+w "$REL"; rm -rf "$REL"; fi
  mkdir -p "$REL"
  git -C "$ROOT" archive "$C" web crates/nebula-core/tests/fixtures/board | tar -x -C "$REL"
  "$VENV/bin/pip" install -q --disable-pip-version-check -r "$REL/web/requirements.txt" >"$LOG/pip.log" 2>&1
  "$VENV/bin/python" -c "import pywebpush, py_vapid, cryptography; print('venv ok')"
  cd "$REL"
  run pnpm --dir web/app install --frozen-lockfile >"$LOG/install.log" 2>&1
  run pnpm --dir web/app build >"$LOG/build.log" 2>&1
  PY=pass UNIT=pass
  run env PYTHONDONTWRITEBYTECODE=1 "$VENV/bin/python" -m unittest discover -s web >"$LOG/python.log" 2>&1 || PY=fail
  run pnpm --dir web/app test:unit >"$LOG/unit.log" 2>&1 || UNIT=fail
  tail -3 "$LOG/python.log"; grep -E "^# (pass|fail)" "$LOG/unit.log"
  [ "$PY" = pass ] && [ "$UNIT" = pass ] || { echo "python $PY, frontend unit $UNIT: no release.json; logs in $LOG" >&2; exit 1; }
  rm -rf web/app/node_modules
  find . -name __pycache__ -prune -exec rm -rf {} +
  "$VENV/bin/python" - "$C" <<'EOF'
import hashlib, json, pathlib, sys
root = pathlib.Path(".")
files = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(root.rglob("*")) if p.is_file()}
json.dump({"commit": sys.argv[1], "verification": {"python": "pass", "frontend_unit": "pass"}, "files": files},
          open("release.json", "w"), indent=1)
EOF
  chmod -R a-w "$REL"
fi

CL="$RELEASES/client-$C"
if [ ! -x "$CL/bin/nebula" ]; then
  (cd "$ROOT" && CARGO_TARGET_DIR="$ROOT/target" run cargo build --release --locked --no-default-features -p nebula >"$LOG/cargo.log" 2>&1) || { tail -30 "$LOG/cargo.log"; exit 1; }
  mkdir -p "$CL/bin"; install -m 555 "$ROOT/target/release/nebula" "$CL/bin/nebula"
fi
"$CL/bin/nebula" --version
echo "release $REL"
