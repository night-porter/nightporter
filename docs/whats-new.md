---
description: "What's new in Night Porter: backlog sweeps for maintainers, handoffs to pi, Cursor, Copilot and more, per-agent models; and in 0.3: notifications on your phone, day and night shifts, PR-comment commands, Forgejo/Gitea, Gemini CLI (experimental) and more handoff targets."
---

# What's new

The full list is in the [changelog](https://github.com/night-porter/nightporter/blob/main/CHANGELOG.md).

## Next release

### Sweep the backlog

A module with three hundred open issues, half of them fixed years ago? `nightporter sweep`
queues a maintainer job for each one, oldest first, and triage now starts by checking whether
the code already does what the issue asks. You get a reply to approve that says "already done,
here's where" so you can close it.

```sh
nightporter sweep project/token --older-than 90d --dry-run
nightporter sweep project/token --older-than 90d
nightporter sweep project/token --prs                # review open MRs from others
```

Filter by `--label`, `--older-than`, `--newer-than` and `--limit`. Every job waits for your
approval, your own PRs still go first, and a sweep never takes the last quarter of the daily
cap. See [Work through the backlog](/advanced#work-through-the-backlog).

### See and clean up worktrees

A **Worktrees** tab (`6`) lists every worktree Night Porter made. `x` removes one, and `u`
brings it back for 7 days, with its commits and uncommitted changes, so cleaning up after a
handoff can't cost you an afternoon's work. From the CLI: `nightporter worktrees`, `rm <job>`,
`restore <job>`. `gc` and the automatic cleanup keep removed work the same way.

### Hand off to pi, Cursor, Copilot and more

`handoff_agent = "pi"` (or `cursor`, `copilot`, `goose`, `qwen`, `droid`, `kiro`) opens a job in
that agent when you take it over. See [Choose the agent](/herdr#choose-the-agent).

### A model per agent

```toml
[defaults.agent_args]
claude = ["--model", "opus"]
codex = ["-m", "gpt-5"]
```

Leave it out and each agent uses its own default. See
[Models per agent](/configuration#models-per-agent).

### git.drupal.org clones

Clones with a `git.drupal.org` remote are found now. For SSH host aliases, add
`aliases = ["github-work"]` to the account.

## 0.3

### Day shift, night shift

A [schedule window](/advanced#working-hours) can now set its own `level`, `agent` and `notify`
list. And notifications can go to your phone or a chat through
[notification targets](/configuration#notification-targets): webhooks (Slack, Discord, ntfy,
JSON) or a command.

Draft and a desktop notification while you work, auto and your phone overnight:

```toml
# ~/.config/nightporter/config.toml
[[notifications.targets]]
name = "phone"
type = "webhook"
format = "ntfy"
url_env = "NIGHTPORTER_NTFY_URL"   # https://ntfy.sh/<a topic only you know>
kinds = ["needs_approval", "failed"]

[defaults]
notify = ["desktop"]

[[defaults.schedule]]
days = "mon-fri"
windows = ["09:00-18:00"]
level = "draft"

[[defaults.schedule]]
days = "daily"
windows = ["18:00-09:00"]
level = "auto"
notify = ["phone"]
```

Set `NIGHTPORTER_NTFY_URL` where the daemon runs (launchd and systemd don't read your shell
profile), subscribe to the topic in the ntfy app, then check it:

```sh
nightporter notify test phone
```

The first open window wins, and the [limits](/configuration#limits-that-config-cannot-change)
still apply: an untrusted (`trust.open`) repo stays at `suggest`, and CI-failure jobs stop at
`draft` unless `ci_auto` is on. On Linux, clicking a desktop notification opens the PR; macOS
can't do that yet.

### Approve from your phone

Comment on the PR or MR:

```
/nightporter approve
```

`dismiss`, `retry` and `status` work too, on GitHub, GitLab and Forgejo/Gitea. Only comments
from your own account count (checked by user ID), and `approve` only releases a job that is
waiting for push approval. See [From your phone](/daily-use#from-your-phone).

### Forgejo and Gitea

Codeberg, gitea.com and self-hosted servers, with a token or your `tea` login:

```toml
[[accounts]]
forge = "forgejo"
host = "codeberg.org"
```

Resolving threads works on Gitea 1.25 and later only; Forgejo has no API for it. See
[Forgejo and Gitea](/configuration#forgejo-and-gitea).

### Gemini CLI (experimental)

`agent = "gemini"` runs Gemini CLI 0.67 or newer, sandboxed with the network off. It hasn't
been run against a real Gemini CLI yet, so treat it as experimental and start with `draft`. See
[Gemini CLI](/advanced#gemini-cli). Cursor CLI and Amp were evaluated and aren't supported; the
[threat model](/threat-model) says why.

### Take over in tmux, Zellij or a new terminal

`h` (or `nightporter handoff <id>`) now also opens jobs in tmux, Zellij or a new terminal
window, or prints the command. `handoff = "auto"` picks the one you're in and never fails: with
nothing around, it prints the command. See [Take over a job](/herdr#take-over-a-job).

### Also

- The Repos tab shows each repo's own level and agent (what `l` and `a` change), and
  `(now auto, from window)` while a window overrides them.
- Coming with the release, in their own repositories: first-party packs for Drupal, Rust and
  accessibility review, and the Night Porter skill as a Claude Code plugin and a Gemini CLI
  extension. These docs will link them once they're published; until then,
  `nightporter skill setup` installs the skill for Claude Code.
