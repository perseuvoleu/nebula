#!/bin/sh
# nebula-sync-skills [--check] [host]  — push the agent skills/config to a remote box.
# --check changes nothing: it prints `differ: N`, the mirrored files whose
# content differs or that exist on one side only (`nebula remote <host> status`).
# Canonical copy: nebula repo scripts/sync-skills.sh (see .agents/skills/sync-skills/SKILL.md).
#
# Mirrors (delete-on-remote, so removals propagate too):
#   ~/Desktop/perseu-stack   the perseu-stack repo; ~/.agents/skills links into its skills/ (relative)
#   ~/.agents/skills          the shared skill source (claude/codex symlink into it)
#   ~/.agents/pstack-models.md  models per role (a relative link into perseu-stack/config)
#   ~/.claude/{skills,commands,agents,rules,shared,CLAUDE.md}
#   ~/.codex/{skills,agents,AGENTS.md}
#   ~/.pi/agent/skills
# Never touches auth/credential/settings files. Symlinks copied as-is; the
# few that point at /Users/<me>/… are rewritten to the remote $HOME.
set -eu
check=
if [ "${1:-}" = "--check" ]; then check=1; shift; fi
host="${1:-findl}"
# .system is Codex's own bundled skills: each machine's Codex rewrites it at startup.
excl="--exclude node_modules --exclude .git --exclude .DS_Store --exclude .system"

# manifest <path> <label>: one "<label>/<file>\t<cksum or link target>" line
# per mirrored file, the same excludes as the sync. Runs on both ends, so
# link targets read home-relative: the sync rewrites /Users/<me>/ links.
manifest_fn='manifest() {
  if [ -L "$1" ]; then printf "%s\t-> %s\n" "$2" "$(readlink "$1" | sed "s#^/Users/[^/]*/#~/#; s#^$HOME/#~/#")"
  elif [ -f "$1" ]; then printf "%s\t%s\n" "$2" "$(cksum <"$1" | cut -d" " -f1-2)"
  elif [ -d "$1" ]; then
    (cd "$1" && find . \( -name node_modules -o -name .git -o -name .DS_Store -o -name .system \) -prune -o -type f -exec cksum {} + \
      | sed -E "s#^([0-9]+) ([0-9]+) \./(.*)\$#$2/\3	\1 \2#")
    (cd "$1" && find . \( -name node_modules -o -name .git -o -name .DS_Store -o -name .system \) -prune -o -type l -print) \
      | while IFS= read -r f; do manifest "$1/$f" "$2/${f#./}"; done
  fi
}'
here_entries= there_entries=

sync() { # sync <local path> <remote path>
  if [ -n "$check" ]; then
    if [ -e "$1" ] || [ -L "$1" ]; then
      here_entries="$here_entries manifest '$1' '$2';"
      there_entries="$there_entries manifest '$2' '$2';"
    fi
    return
  fi
  if [ -d "$1" ]; then
    ssh "$host" "mkdir -p '$2'"
    rsync -az --delete --links $excl "$1/" "$host:$2/"
  elif [ -f "$1" ]; then
    ssh "$host" "mkdir -p '$(dirname "$2")'"
    rsync -az "$1" "$host:$2"
  fi
}

sync ~/Desktop/perseu-stack  Desktop/perseu-stack
sync ~/.agents/skills        .agents/skills
sync ~/.agents/pstack-models.md .agents/pstack-models.md
sync ~/.claude/skills        .claude/skills
sync ~/.claude/commands      .claude/commands
sync ~/.claude/agents        .claude/agents
sync ~/.claude/rules         .claude/rules
sync ~/.claude/shared        .claude/shared
sync ~/.claude/CLAUDE.md     .claude/CLAUDE.md
sync ~/.codex/skills         .codex/skills
sync ~/.codex/agents         .codex/agents
sync ~/.codex/AGENTS.md      .codex/AGENTS.md
sync ~/.pi/agent/skills      .pi/agent/skills

if [ -n "$check" ]; then
  here=$(mktemp) there=$(mktemp)
  trap 'rm -f "$here" "$there"' EXIT
  sh -c "$manifest_fn; $here_entries" | LC_ALL=C sort >"$here"
  # Remote paths are home-relative: ssh starts in the remote $HOME.
  printf '%s\n' "$manifest_fn; $there_entries" | ssh "$host" sh -s >"$there"
  LC_ALL=C sort -o "$there" "$there"
  n=$( { LC_ALL=C comm -23 "$here" "$there"; LC_ALL=C comm -13 "$here" "$there"; } | cut -f1 | sort -u | wc -l)
  echo "differ: $(echo $n)"
  exit 0
fi

# Absolute symlinks into this Mac's home would dangle on Linux.
ssh "$host" 'for l in $(find ~/.claude/skills ~/.codex/skills ~/.pi/agent/skills -maxdepth 1 -type l 2>/dev/null); do
  t=$(readlink "$l"); case "$t" in /Users/*/.agents/*) ln -sfn "$HOME/.agents/${t#/Users/*/.agents/}" "$l";; esac; done
echo "skills on $(hostname): $(ls ~/.claude/skills | wc -l) claude, $(ls ~/.codex/skills | wc -l) codex, $(find ~/.claude/skills ~/.codex/skills -maxdepth 1 -xtype l | wc -l) broken links"'
