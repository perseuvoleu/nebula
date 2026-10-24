# Worktrees

A user or an agent creates a worktree for a new branch from the CLI or the TUI; nebula checks it out beside the project and draws it as a row under the primary checkout.

## Sub-features

- `wt-new`: `worktree new` turns spaces into hyphens and branches from the primary checkout's HEAD.
- `wt-tui-new`: `n` in the WORKTREES & BRANCHES column asks for the kind, the name and the base branch, then creates the same worktree.
- `wt-row`: the sidebar shows the worktree as `(wt)` with `from <base>` under it.

## How to get to it (user POV)

- Run `nebula worktree new --project <project> <name words>` in a terminal.
- Press `n` with the WORKTREES & BRANCHES column focused (footer hint `n: new worktree`).

## Driving it with vn

Preconditions:

- The baseline from the README holds.

- **Create.** Run `vn nebula worktree new --project repo fix login`. Stdout is `{"branch":"fix-login",…,"path":"<state>/repo-worktrees/fix-login",…}`.
- **Record.** Run `vn nebula worktree list --project repo > "$NV_EVIDENCE/worktrees.json"`. It lists `main` (`is_main: true`) and `fix-login` (`created_from: "main"`).
- **See the row.** Run `vn tui`, `vn wait fix-login 20` and `vn shot worktree-row`. A row reads `○ fix-login (wt)` with `from main` under it.
- **Create from the TUI.** Run `vn tui`, then `vn keys Tab Tab Tab`: the footer starts `n: new worktree`. Run `vn shot wt-tui-before`. Press `vn keys n`; a `New worktree or branch` menu opens on `Worktree (own directory)`. Press `vn keys Enter`; a `New worktree` prompt asks for the branch name. Type `vn keys -l 'tui wt'` and `vn keys Enter`; a `Base branch` picker lists `main ⌂ primary`. Press `vn keys Enter`, then `vn wait tui-wt 20` and `vn shot wt-tui-after`: a row reads `○ tui-wt (wt)` with `from main` under it. Save `vn nebula worktree list --project repo` beside the shots; it lists `tui-wt` with `created_from: "main"`.

## Gotchas

- The column truncates at about 13 characters of branch name (`fix-login-flow` draws as `fix-login-f…`), so `vn wait fix-login-flow` never matches. Use a short name.
- The worktree directory is `<state>/repo-worktrees/<branch>`, a sibling of the repo; `vn down` removes it with the state dir.
