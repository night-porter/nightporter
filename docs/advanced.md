---
description: "Night Porter recipes: levels per repo, working hours, trust rules, limits and budgets, sandboxing, DDEV and Docker environments, maintainer mode and replies."
---

# Advanced recipes

Each recipe goes in `~/.config/nightporter/config.toml` under `[defaults]`, or in a repo file
(`repos/<forge>/<owner>/<repo>.toml`) to apply to one repo. The daemon picks up changes when you
save. See the [configuration reference](/configuration) for every key.

## Different levels per repo

Let it run on its own on a side project, but only suggest on the one that pays the bills:

```toml
# config.toml
[defaults]
level = "draft"
```

```toml
# repos/github/me/dotfiles.toml
subscribed = true
level = "auto"
```

```toml
# repos/github/acme/payments.toml
subscribed = true
level = "suggest"
[checks]
protected = ["migrations/**", "src/billing/**"]
```

## Working hours

Several windows a day, a window across midnight, and a different timezone:

```toml
[[defaults.schedule]]
days = "mon-fri"
windows = ["08:00-09:00", "17:00-22:00"]

[[defaults.schedule]]
days = "sat,sun"
windows = ["22:00-06:00"]
tz = "Europe/Zurich"
```

Events outside the windows are queued and run when the next window opens.

A window can also change the `level`, `agent` and `notify` list while it is open. Draft and a
desktop notification during the day, auto and your phone overnight:

```toml
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
notify = ["phone"]          # a [[notifications.targets]] name
```

The first window that is open wins. What it doesn't set comes from the repo's settings. Names in
`notify` must be `desktop` or a target, or the config won't load. The level is chosen when a
job is created, the agent when it starts, and the `notify` list when the notification is sent.
A window's `level` can't get past the [limits](/configuration#limits-that-config-cannot-change):
with `trust.open` it still stops at `suggest`, and CI-failure jobs still stop at `draft` unless
`ci_auto` is on. The TUI's schedule field only edits days and times; use `e` for the rest.

## Who it listens to

```toml
[defaults.trust]
roles = ["owner", "maintainer", "member", "collaborator"]
allow_users = ["alice"]
deny_users = ["mallory"]
allow_teams = ["acme/core"]
allow_bots = ["coderabbitai[bot]"]
```

A repo file can add people to these lists, but it can’t remove a global deny. Everyone else
goes to the inbox.

Bots are never trusted by role. A bot that isn’t in `allow_bots` is dropped, not sent to the inbox,
so add review bots like CodeRabbit by name if you want Night Porter to act on them.

`trust.open = true` lets anyone through, and caps the level at `suggest`.

## Only some triggers

```toml
# repos/github/acme/web.toml
subscribed = true
triggers = ["ci_failure", "merge_conflict"]   # leave review comments to humans
```

The triggers are `review_comment`, `review`, `comment`, `ci_failure` and `merge_conflict`.

## Limits and budgets

```toml
# config.toml
max_concurrent_jobs = 2
daily_job_cap = 50
min_free_gb = 5

[defaults]
timeout = "30m"
max_budget_usd = 2.0      # Claude Code only
max_ci_attempts = 3
```

## Agent options

```toml
[defaults]
agent = "claude"                                # or "opencode", "codex", "gemini"
agent_args = ["--model", "opus"]
allow_commands = ["npm test", "npm run lint"]   # prefix match, Claude Code and Gemini CLI
prompt = "prompts/house-style.md"               # relative to the config dir
```

`nightporter repo <owner/name>` shows what all of this adds up to for one repo, including the
contents of your prompt files and the repo's own `CLAUDE.md` / `AGENTS.md`.

## Sandboxing

With Claude Code, every shell command the agent runs goes in the OS sandbox (Seatbelt on macOS,
bubblewrap on Linux). If the sandbox can’t start, the job doesn’t run.

Commands can't read your home folder, apart from the worktree and common toolchain folders
(`~/.cargo`, `~/.rustup`, `~/.npm`, `~/.nvm`, `~/go`, `~/.composer`, `~/.cache` and similar).
Credential files inside those (`~/.cargo/credentials.toml`, `~/.composer/auth.json`, …) stay
blocked. If a build needs something else, allow that folder:

```toml
[defaults]
sandbox_read_allow = ["~/.config/mytool"]
```

The agent gets its own git config with only your name and email (as git would use them in that
repo), so your `~/.gitconfig` with its credential helpers stays out of reach.

Commands have no network access unless you allow a domain:

```toml
[defaults]
sandbox_domains = ["registry.npmjs.org"]
```

Tests that need Docker or DDEV sockets don’t work inside the sandbox. Turn it off for that repo
only:

```toml
# repos/gitlab/project/token.toml
sandbox = false
```

### Codex

With `agent = "codex"`, Night Porter runs `codex exec` in Codex's own OS sandbox:
`--sandbox workspace-write` with the network off, `read-only` for review jobs, and
`danger-full-access` only when `sandbox = false`. Your `~/.codex/config.toml` and execpolicy rules
are ignored (`--ignore-user-config --ignore-rules --ephemeral`); the login still comes from
`CODEX_HOME`. Codex has no per-command allow list, so `allow_commands` and `sandbox_domains`
don't apply, and `max_budget_usd` is Claude Code only. Install it with
`npm install -g @openai/codex` or `brew install --cask codex`, then run `codex` once to log in.

### Gemini CLI

::: warning Experimental
Gemini CLI support is new in 0.3 and hasn't been run against a real Gemini CLI yet, only
against tests. Try it on a repo where `draft` or `suggest` is the level, and
[report](/reporting-issues) what you see.
:::

With `agent = "gemini"`, Night Porter runs `gemini -p` (0.67 or newer; older versions are
refused because they ignore the sandbox setting) with:

- its own system settings file (`GEMINI_CLI_SYSTEM_SETTINGS_PATH`, which wins over your
  `~/.gemini/settings.json`): Gemini's tool sandbox on (Seatbelt on macOS, bubblewrap on Linux),
  network off for commands, hooks off, auto-update off, the repo's `.env` not loaded;
- its own admin policy (`--admin-policy`, which wins over your policies): only the git
  subcommands Night Porter allows plus your `allow_commands`, the usual deny list, no web tools,
  and no command may ask to widen the sandbox;
- `--approval-mode default` (never YOLO), `--extensions none` and no MCP servers.

The sandbox lets commands read the worktree, its git dir and the folders on your `PATH` (plus
their parent when they end in `bin`, e.g. `~/.cargo`), and write the worktree and temp folders.
`sandbox_read_allow` and `sandbox_domains` don't apply. A branch that carries
`.gemini/settings.json` or `.gemini/.env` isn't run with Gemini CLI: headless Gemini has to trust
the workspace, and it would load those files. `sandbox = false` turns the tool sandbox off.
Gemini CLI itself won't edit build files (`package.json`, lockfiles, `Cargo.toml`,
`Makefile`, `Dockerfile`, …) without a person confirming, so in these unattended runs it can't
change them. Gemini CLI reports no cost, so `max_budget_usd` doesn't apply. Install it with
`npm install -g @google/gemini-cli`; it needs a paid `GEMINI_API_KEY` in the daemon's
environment, or a Gemini Code Assist Standard/Enterprise login (Google moved consumer accounts to
Antigravity in June 2026).

Cursor CLI and Amp aren't supported: neither can run a job with only Night Porter's settings
(both load sandbox, hook or permission files from the repo, which override yours) and a sandbox
that fails closed. See the [threat model](/threat-model#agent-sandboxes).

OpenCode has no sandbox.

## Environments (DDEV, Docker, …)

Start a stack before the agent runs and stop it afterwards. Each worktree gets its own:

```toml
# repos/gitlab/project/token.toml
subscribed = true
allow_commands = ["ddev phpunit", "ddev exec phpcs"]
[env]
setup = ["ddev config --project-type=drupal11 --project-name=np-{repo}-{n} --auto", "ddev start", "ddev composer install"]
teardown = ["ddev stop"]
cleanup = ["ddev delete -O -y"]
prompt = "prompts/drupal.md"
```

`teardown` runs after each job, `cleanup` when the worktree is removed.

## Maintainer mode

Work on other people’s issues and MRs, but only when a trusted person asks: with the label, or a
`/nightporter triage`, `reproduce`, `fix` or `review` comment.

```toml
[defaults.maintainer]    # or [maintainer] in a repo file
enabled = true
label = "nightporter"
```

Maintainer mode is capped at `draft`, so you always approve before anything is pushed or posted.

- The label means the same as `/nightporter fix`.
- `fix` on an issue pushes a new `nightporter/issue-<n>` branch and opens a **draft** PR/MR that
  closes the issue.
- `triage`, `review` and `reproduce` only reply; they never push code.
- When the work would run someone else’s code (their PR, or `reproduce` with an environment or
  allowed commands), the job first waits as **NEEDS RUN APPROVAL**. Check the build files it lists,
  then press `a` to let it run on that exact commit.
- `review` doesn't run the PR's code, so with Claude Code or Gemini CLI it starts without that
  approval: no tests, no environment, read-only. Set `review_runs_code = true` to have reviews run
  your `allow_commands` (tests) too, after you approve the run. Codex and OpenCode reviews always
  wait for approval.
- `triage` first checks whether the code already does what the issue asks. If it does, the reply
  says so, with the files or commits that show it, and recommends closing the issue. Night Porter
  never closes it for you.

### Review requests

When someone asks for your review on a GitHub PR, Night Porter can prepare one: the agent reads
the PR and writes a review (summary, issues with file and line, security, tests, a verdict)
that waits for you in the TUI.

```toml
[maintainer]
enabled = true
review_requests = true
level = "suggest"          # optional: copy-paste text, nothing is posted
```

It runs once per PR head while your review is still requested, so a re-request after new pushes
gets a fresh review, and none is made once you've submitted yours. At `draft` (the default),
approving the job posts the review as a PR comment. GitHub only for now.

### Work through the backlog

`nightporter sweep` queues a maintainer job for each open issue in a repo (or each open PR/MR by
someone else, with `--prs`), oldest first. It's how you go through a module with hundreds of
open issues and find the ones that are already done.

```sh
nightporter sweep project/token --dry-run                    # list what would be queued
nightporter sweep project/token --older-than 90d             # triage everything older than 3 months
nightporter sweep project/token --newer-than 14d --verb fix  # draft MRs for the last two weeks
nightporter sweep project/token --label "Needs work" --label Bug --limit 20
nightporter sweep project/token --prs                        # review open MRs from others
```

| flag | what it does |
|---|---|
| `--verb` | `triage` (default for issues), `reproduce`, `fix` (draft PR/MR per issue) or `review` (default with `--prs`) |
| `--label` | only items with this label; repeat it and all must match |
| `--older-than`, `--newer-than` | only items opened more than / less than this long ago (`90d`, `14d`, `12h`) |
| `--limit` | at most this many, oldest first |
| `--dry-run` | list them, queue nothing (works before the repo is subscribed) |

The repo must be subscribed with maintainer mode on. Sweep jobs go through the same queue as
everything else: your schedule, `max_concurrent_jobs` and `daily_job_cap` apply, and every reply
or draft MR waits for your approval. Your own PRs always go first, and sweep jobs never use the
last quarter of the daily cap, so a big backlog can't hold up review comments on your PRs. A
large backlog takes a few days at the default cap of 50; raise `daily_job_cap` to go faster.
Running the same sweep again only queues items it hasn't queued before.

For a sweep that only reads, set `level = "suggest"` under `[maintainer]`: triage replies then
show up in the TUI as copy-paste text and nothing is posted.

## Replies

```toml
[defaults.reply]
style = "concise"          # detailed-tech-lead, non-technical, or one from an enabled pack
language = "German"
footer = true              # "via Night Porter (Claude Code)"
resolve_threads = false
```

## Packs

Packs add review steps, reply styles and environment presets. They’re pinned to a commit, and
they can only make the rules stricter.

```sh
nightporter market add <git-url>         # a marketplace: a git repo with a marketplace.toml
nightporter market list
nightporter pack add <name>              # from a marketplace
nightporter pack add <git-url>@<sha>     # or straight from git, pinned
nightporter pack list
nightporter pack env <pack>/<env>        # print an [env] preset to paste into a repo file
```

```toml
[defaults]
packs = ["a11y"]
```

To write your own, see [Writing packs](/writing-packs).

## Testing a config

`NIGHTPORTER_HOME=/some/dir` moves the config and the state to `/some/dir/{config,data}`, so you
can try things without touching your real setup.
