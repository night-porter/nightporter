---
description: "What's new in Night Porter 0.4 and 0.3: real PR reviews, backlog sweeps, a Worktrees tab with undo, more handoff targets and phone notifications."
---

# What's new

The full list is in the [changelog](https://github.com/night-porter/nightporter/blob/main/CHANGELOG.md).

## 0.4

### Every ready PR, real reviews (0.4.5)

**Watch every ready PR.** `watch_prs = true` under `[maintainer]` puts each ready (not draft)
PR/MR by someone else in the inbox, not just the ones where your review was requested. Press
`o` to queue a review, `i` to skip it.

**Approve or request changes.** With `review_verdicts`, a review job is posted as a real
review with the agent's outcome instead of a comment. It always waits for you to approve it.

```toml
[maintainer]
enabled = true
watch_prs = true
review_verdicts = ["request_changes", "approve"]
```

See [Approve or request changes](/advanced#approve-or-request-changes) and
[Watch every ready PR](/advanced#watch-every-ready-pr).

### Review bots, tests that actually run, signed commits (0.4.4)

**The whole bot review, not one comment.** With a review bot in `allow_bots`, a job on your own
PR now reads the PR's review threads and takes the bot's unresolved ones, including comments
from before you allowed it. A failing "review score" check (Greptile's confidence gate) used to
start a job that saw only the check's title. After the push, Night Porter asks Greptile to
review again, and stops after three rounds per PR. GitHub only for now.

```toml
[defaults.trust]
allow_bots = ["greptile-apps[bot]"]
```

**Dependencies installed, checks run.** `install = true` under `[env]` installs from your
lockfiles before the agent starts (composer, npm, pnpm, yarn, cargo), and
`allow_commands = ["*"]` lets the agent run any command inside the sandbox. It then runs the
tests, linters and static analysis that cover its change, the way your CI does.

**Signed commits.** If your git config signs commits, Night Porter signs the agent's commits with
your own key before you review them. The agent never sees the key.

See [Review bots](/advanced#review-bots), [Dependencies and tests](/advanced#dependencies-and-tests)
and [Signed commits](/advanced#signed-commits).

### Let jobs fix `.env.example` on your own repos (0.4.3)

`.env.*` files are protected, so a review nit on `.env.example` used to wait for you. On repos
you maintain, opt in per repo:

```toml
[checks]
allow_env_variants = true
```

Jobs can then edit `.env.example`, `.env.dist` and other `.env.*` files. A bare `.env` stays
protected and the secrets scan still checks every added line. Also new: approving a
`suggest`-level job now says nothing was pushed or posted, instead of just "marked done".

### Reviews when someone asks for yours (0.4.1)

When someone requests your review on a GitHub PR, Night Porter can prepare one: a summary,
issues with file and line, security, tests and a verdict, waiting for you in the TUI. Turn it on
per repo with `review_requests = true` under `[maintainer]`. With Claude Code or Gemini CLI a
review only reads the code (no tests, no environment), so it starts without waiting for you to
approve running someone else's code. See [Review requests](/advanced#review-requests).

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
handoff can't cost you an afternoon's work. A new job on the PR keeps your edits before it
resets the worktree, too. From the CLI: `nightporter worktrees`, `rm <job>`,
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
