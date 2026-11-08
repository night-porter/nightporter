---
description: "Fix Night Porter problems: start with nightporter doctor, then what to check when a job does not start, fails or keeps waiting."
---

# Troubleshooting

## Start with doctor

```sh
nightporter doctor
```

It checks your config, the tools, your `gh`/`glab` logins and Forgejo/Gitea tokens, the code directory, free disk space
and whether the daemon is running. Every problem comes with a `fix:` line.

## Read the log

On macOS the daemon writes to `~/.local/share/nightporter/daemon.log`. On Linux it logs to the
journal: `journalctl --user -u nightporter`. For more detail, set `NIGHTPORTER_LOG=nightporter=debug`
and run `nightporter daemon` in a terminal.

For one job, run `nightporter show <id>`: it has the checks, replies, diff and the agent’s log.

## A job never starts

Work through these in order:

- **Is the daemon running?** `nightporter doctor` says, and the TUI header says **NOT RUNNING**.
  Start it with `nightporter start` (or `D` in the TUI); `nightporter service install` also starts
  it at login. To watch it, run `nightporter daemon` in a terminal (if one is already running, it
  says so and exits).
- **Is the repo subscribed?** `nightporter repos` (or `nightporter repo <owner/name>` for
  everything that applies to it). `nightporter repos --all` lists unwatched clones too. If the repo is missing, check `code_dir` and
  `scan_depth`, and that the clone’s remote points at a forge in your `[[accounts]]`.
- **Is the trigger enabled?** Check `triggers` for that repo.
- **Is the author trusted?** Look in the inbox (`2` in the TUI, or `nightporter inbox list`). Bots
  that aren’t in `allow_bots` are dropped, not sent to the inbox.
- **Is it outside your working hours?** Events outside the `schedule` wait for the next window.
- **Is it paused?** `nightporter resume`.
- **Did it hit a limit?** The log says `daily job cap reached` or `only N GB free`. Raise
  `daily_job_cap`, free some disk, or lower `min_free_gb`.
- **Is it still in the quiet period?** New activity resets the `debounce` timer.

## A job failed

- `nightporter show <id>` has the agent’s log.
- **Timeout**: raise `timeout` for that repo.
- **Sandbox on Linux**: install `bubblewrap` and `socat`.
- **Tests need Docker or DDEV**: set `sandbox = false` for that repo. See
  [Sandboxing](/advanced#sandboxing).
- **Commands blocked**: add them to `allow_commands` (Claude Code). Codex has no allow list: its
  sandbox allows writes to the worktree only and no network; `sandbox = false` lifts that.

Fix the cause, then `nightporter retry <id>`.

## A check blocked the push

Secrets, protected files and history rewrites are blocking checks. Look at the diff with
`nightporter show <id>`. If the change is fine, approve it anyway with `A` in the TUI or
`nightporter approve <id> --override-blocking`. Otherwise reject it.

Dependency and size changes ask before pushing by default (`checks.escalate_on`).

## My config change did nothing

The daemon reloads on save. If the file is invalid, it keeps the previous config and logs
`config not reloaded`. Unknown keys are errors, so a typo shows up there and in
`nightporter doctor`.

## A job I dismissed or rejected is gone

It's in the **History** tab (`3`): press `/` and type `dismissed` or the repo name. `u` restores
a dismissed job, or run `nightporter restore <id>`. Only `X` / `nightporter purge` deletes jobs
for good. An inbox event you ignored: `nightporter inbox list --ignored`, then
`nightporter inbox restore <event>` (or `u` in the Inbox right after ignoring).

## Disk filling up with worktrees

Idle worktrees are removed after `worktree_idle_days` (14). To clean up now:

```sh
nightporter gc          # idle ones
nightporter gc --all    # every worktree not in use
```

## Still stuck?

[Open an issue](/reporting-issues).
