#!/bin/sh
# nebula-sync-skills [host]  — push the agent skills/config to a remote box.
# Canonical copy: nebula repo scripts/sync-skills.sh (see .claude/skills/sync-skills/SKILL.md).
#
# Mirrors (delete-on-remote, so removals propagate too):
#   ~/.agents/skills          the shared skill source (claude/codex symlink into it)
#   ~/.claude/{skills,commands,agents,rules,shared,CLAUDE.md}
#   ~/.codex/{skills,agents,AGENTS.md}
#   ~/.pi/agent/skills
# Never touches auth/credential/settings files. Symlinks copied as-is; the
# few that point at /Users/<me>/… are rewritten to the remote $HOME.
set -eu
host="${1:-findl}"
excl="--exclude node_modules --exclude .git --exclude .DS_Store"

sync() { # sync <local path> <remote path>
  if [ -d "$1" ]; then
    ssh "$host" "mkdir -p '$2'"
    rsync -az --delete --links $excl "$1/" "$host:$2/"
  elif [ -f "$1" ]; then
    ssh "$host" "mkdir -p '$(dirname "$2")'"
    rsync -az "$1" "$host:$2"
  fi
}

sync ~/.agents/skills        .agents/skills
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

# Absolute symlinks into this Mac's home would dangle on Linux.
ssh "$host" 'for l in $(find ~/.claude/skills ~/.codex/skills ~/.pi/agent/skills -maxdepth 1 -type l 2>/dev/null); do
  t=$(readlink "$l"); case "$t" in /Users/*/.agents/*) ln -sfn "$HOME/.agents/${t#/Users/*/.agents/}" "$l";; esac; done
echo "skills on $(hostname): $(ls ~/.claude/skills | wc -l) claude, $(ls ~/.codex/skills | wc -l) codex, $(find ~/.claude/skills ~/.codex/skills -maxdepth 1 -xtype l | wc -l) broken links"'
