---
name: sync-skills
description: "Push the user's agent skills and global instructions (claude, codex, pi) to a remote nebula host so sessions there see the same skills as the laptop. Trigger: \"sync skills\", \"update skills on the server / on findl\", \"copy my skills remote\", \"nss\", or a remote agent complaining a skill is missing."
user-invocable: true
---

Remote nebula projects (`nebula add host:/path`) run the agent CLI on the far host, which reads
skills from *its* home. Nothing keeps that home in step with the laptop — this does. It is a plain
rsync mirror, `scripts/sync-skills.sh`, also on PATH as `nebula-sync-skills` (alias `nss`).

## Run it

```bash
scripts/sync-skills.sh            # default host: findl
scripts/sync-skills.sh <host>     # any ssh destination / ~/.ssh/config alias
```

It prints one line from the remote at the end: `skills on <host>: N claude, M codex, K broken links`.
Compare N/M with the laptop (`ls ~/.claude/skills | wc -l`, `ls ~/.codex/skills | wc -l`); `broken
links` should match the laptop's own dangling symlinks (check with
`find ~/.claude/skills ~/.codex/skills -maxdepth 1 -type l ! -exec test -e {} \; -print`), and
a higher number means the source dir for that symlink wasn't mirrored — add it to the script.

## What it mirrors

| Local | Remote | Why |
|---|---|---|
| `~/.agents/skills` | `~/.agents/skills` | the shared skill source; claude/codex entries symlink into it |
| `~/.claude/{skills,commands,agents,rules,shared,CLAUDE.md}` | same | Claude Code's global config; `CLAUDE.md` `@include`s `shared/` and `commands/shared/` |
| `~/.codex/{skills,agents,AGENTS.md}` | same | Codex's global skills, subagent definitions, instructions |
| `~/.pi/agent/skills` | same | Pi skills |

Directories sync with `--delete` (a skill removed locally disappears remotely) and skip
`node_modules`, `.git`, `.DS_Store`. Symlinks are copied as symlinks; ones that point at
`/Users/<me>/.agents/…` are rewritten to the remote `$HOME` afterwards, so the mirror works on Linux.

## What it must never touch

`auth.json`, `.credentials.json`, `settings.json`, `config.toml`, session/history databases — these
hold credentials or machine-local paths. The script only names the entries in the table above; do
not widen it to whole `~/.claude` or `~/.codex` directories.

## Adding an entry

Add one `sync <local> <remote>` line to the script — it handles a file or a directory and creates the
remote parent. If the new entry is a symlink target used by another entry, place it *before* the
symlink's directory so the link resolves on the first run.

## Failure shapes

- `rsync: command not found` on the remote → `ssh <host> 'sudo -n apt-get install -y rsync'`
  (Debian/Ubuntu); the script needs rsync on both ends.
- `Permission denied (publickey)` → the host isn't set up for key auth from this machine; the
  script never prompts (BatchMode), fix `~/.ssh/config` first.
- A skill present locally but not remote after a run → it lives somewhere outside the table
  (check `readlink` on the local entry) and needs its own `sync` line.
