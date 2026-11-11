#!/bin/sh
set -eu

hook="$(cd "$(dirname "$0")" && pwd)/pre-commit"
root="$(dirname "$(dirname "$hook")")"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
failures=0

fail() {
  echo "FAIL: $1" >&2
  failures=$((failures + 1))
}

new_repo() {
  repo="$tmp/$1"
  git init -q "$repo"
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name test
  mkdir -p "$repo/.githooks"
  cp "$hook" "$repo/.githooks/pre-commit"
  cp "$hook" "$repo/.git/hooks/pre-commit"
}

formatted='fn main() {
    let x = 1;
    println!("{x}");
}
'
unformatted='fn main(){let x=1;println!("{x}");}
'

new_repo unformatted
mkdir -p "$repo/src"
printf '%s' "$unformatted" > "$repo/src/bad file.rs"
git -C "$repo" add .
if out="$(git -C "$repo" commit -q -m bad 2>&1)"; then
  fail "a commit staging an unformatted .rs file was accepted"
else
  case "$out" in *"src/bad file.rs"*) ;; *) fail "refusal does not name src/bad file.rs: $out" ;; esac
  case "$out" in *"sh .githooks/pre-commit fix"*) ;; *) fail "refusal does not name the fix: $out" ;; esac
fi

new_repo child-module
mkdir -p "$repo/src"
printf 'mod other;\n%s' "$unformatted" > "$repo/src/main.rs"
printf 'fn other(){}\n' > "$repo/src/other.rs"
git -C "$repo" add src/main.rs
if out="$(git -C "$repo" commit -q -m child 2>&1)"; then
  fail "a commit staging an unformatted src/main.rs was accepted"
else
  (cd "$repo" && sh .githooks/pre-commit fix) || fail "the printed fix failed from the repo root"
  [ "$(cat "$repo/src/other.rs")" = 'fn other(){}' ] || fail "the printed fix rewrote the child module src/other.rs"
  rustfmt < "$repo/src/main.rs" | cmp -s - "$repo/src/main.rs" || fail "the printed fix left src/main.rs unformatted"
  git -C "$repo" commit -q -m child 2>&1 || fail "the commit was refused after the printed fix"
fi

new_repo partially-staged
printf '%s' "$unformatted" > "$repo/m.rs"
git -C "$repo" add m.rs
printf '%sfn other_session() {}\n' "$unformatted" > "$repo/m.rs"
if git -C "$repo" commit -q -m partial > /dev/null 2>&1; then
  fail "a commit staging an unformatted m.rs was accepted"
else
  (cd "$repo" && sh .githooks/pre-commit fix) || fail "the printed fix failed on a partially staged file"
  git -C "$repo" commit -q -m partial 2>&1 || fail "the commit was refused after the printed fix"
  git -C "$repo" show HEAD:m.rs | grep -q other_session && fail "the printed fix committed the unstaged edit"
  git -C "$repo" show HEAD:m.rs | grep -q 'let x = 1;' || fail "the printed fix did not format the staged m.rs"
  [ "$(cat "$repo/m.rs")" = "$(printf '%sfn other_session() {}' "$unformatted")" ] || fail "the printed fix changed the working copy of a partially staged file"
  [ -z "$(git -C "$repo" ls-files -o -- ':!.githooks')" ] || fail "the printed fix left stray files: $(git -C "$repo" ls-files -o -- ':!.githooks')"
fi

new_repo vendor
mkdir -p "$repo/vendor/vt100/src"
printf '%s' "$unformatted" > "$repo/vendor/vt100/src/attrs.rs"
git -C "$repo" add vendor
git -C "$repo" commit -q -m vendor 2>&1 || fail "an unformatted file under vendor/, which cargo fmt excludes, was refused"

new_repo parse-error
printf 'fn main( {\n' > "$repo/broken.rs"
git -C "$repo" add broken.rs
if out="$(git -C "$repo" commit -q -m broken 2>&1)"; then
  fail "a commit staging an unparsable .rs file was accepted"
else
  case "$out" in *"rustfmt cannot parse broken.rs"*) ;; *) fail "parse error not reported as one: $out" ;; esac
  case "$out" in *"error"*"<stdin>"*) ;; *) fail "rustfmt's diagnostic was dropped: $out" ;; esac
fi

new_repo no-rustfmt
nofmt="$tmp/nofmt-bin"
mkdir -p "$nofmt"
ln -s "$(command -v git)" "$nofmt/git"
printf '%s' "$formatted" > "$repo/main.rs"
git -C "$repo" add main.rs
if out="$(PATH="$nofmt:/usr/bin:/bin" git -C "$repo" commit -q -m nofmt 2>&1)"; then
  fail "a commit staging a .rs file passed without rustfmt"
else
  case "$out" in *"rustfmt not found"*) ;; *) fail "missing rustfmt not reported as such: $out" ;; esac
fi

[ "$(grep '^edition' "$root/rustfmt.toml")" = "$(grep '^edition' "$root/Cargo.toml")" ] || fail "rustfmt.toml and Cargo.toml name different editions"

new_repo edition
printf 'edition = "2021"\n' > "$repo/rustfmt.toml"
printf 'async fn f() {}\n' > "$repo/lib.rs"
git -C "$repo" add .
git -C "$repo" commit -q -m edition 2>&1 || fail "the hook ignored the edition in rustfmt.toml"

new_repo staged-formatted
printf '%s' "$formatted" > "$repo/main.rs"
git -C "$repo" add main.rs
printf '%s' "$unformatted" > "$repo/main.rs"
git -C "$repo" commit -q -m good 2>&1 || fail "a formatted staged .rs file was refused over its unstaged working-tree edits"

new_repo no-rust
stub="$tmp/stub-bin"
mkdir -p "$stub"
for tool in cargo rustfmt; do
  printf '#!/bin/sh\ntouch "%s/%s-ran"\nexit 1\n' "$tmp" "$tool" > "$stub/$tool"
  chmod +x "$stub/$tool"
done
echo notes > "$repo/README.md"
git -C "$repo" add README.md
PATH="$stub:$PATH" git -C "$repo" commit -q -m docs 2>&1 || fail "a commit with no .rs file was refused"
for tool in cargo rustfmt; do
  [ ! -e "$tmp/$tool-ran" ] || fail "a commit with no .rs file ran $tool"
done

if [ "$failures" -ne 0 ]; then
  echo "$failures failure(s)" >&2
  exit 1
fi
echo "pre-commit: all checks passed"
