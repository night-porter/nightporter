---
description: "Every Night Porter setting: config.toml, per-repo override files, defaults, trust rules, schedules, checks and the limits config cannot change."
---

# Configuration reference

You rarely need to edit these files by hand: `nightporter setup` (or `S` in the TUI) walks you
through the basics, and the TUI's **Settings** tab (`5`) shows and changes every key below,
checking each change before it's saved. `e` there opens the file in your editor, with the same
check. To see what actually applies to one repo (its file, the defaults, prompt files and
packs together), use `nightporter repo <owner/name>` or `Enter` on the Repos tab.

Files:

| file | purpose |
|---|---|
| `~/.config/nightporter/config.toml` | global settings, accounts and `[defaults]` |
| `~/.config/nightporter/repos/<forge>/<owner>/<repo>.toml` | per-repo overrides (any `[defaults]` key, plus `subscribed`) |
| `~/.config/nightporter/packs.lock` | installed packs, pinned |
| `~/.local/share/nightporter/` | state database, job files, worktrees, packs, daemon log |

`NIGHTPORTER_HOME=/some/dir` moves both to `/some/dir/{config,data}` (handy for testing).
Other environment variables: `NIGHTPORTER_LOG` (log filter, e.g. `nightporter=debug`),
`NIGHTPORTER_NO_NOTIFY` (no notifications at all) and `NO_COLOR` (no colours in the TUI).

Repo files are keyed by forge, not host, so a gitlab.com repo and a self-hosted GitLab repo with
the same `owner/repo` path share one file.
The daemon reloads config files when they change. An invalid file is reported and the previous
config stays active. Unknown keys are errors, so typos don't fail silently.

## Global keys

| key | default | description |
|---|---|---|
| `version` | `1` | config format version |
| `code_dir` | `~/coding` | where your clones live |
| `scan_depth` | `3` | how many folders deep to look for clones (`~/coding/drupal/token` is 2). A clone is matched to an account by its remote's host; see `aliases` below if one is missing from `nightporter repos --all` |
| `poll_interval` | `2m` | never faster than the forge's `X-Poll-Interval` |
| `sweep_interval` | `10m` | open-PR sweep for merge conflicts and CI |
| `min_free_gb` | `5` | jobs wait when free disk is below this |
| `max_concurrent_jobs` | `2` | across all repos |
| `daily_job_cap` | `50` | jobs per 24h |
| `retention_days` | `30` | history kept in the state db |
| `worktree_idle_days` | `14` | idle worktrees are removed after this |
| `marketplaces` | `[]` | extra marketplace git URLs |
| `handoff_agent` | `job` | which agent a handoff starts: `job` (the one that ran it), `claude`, `opencode`, `codex`, `gemini`, or a handoff-only agent: `pi`, `cursor`, `copilot`, `goose`, `qwen`, `droid`, `kiro` ([details](/herdr#choose-the-agent)) |
| `handoff` | `auto` | where `h` opens a job: `auto`, `herdr`, `rex`, `tmux`, `zellij`, `terminal` or `print` ([details](/herdr#take-over-a-job)) |
| `pr_commands` | `true` | act on `/nightporter approve`, `dismiss`, `retry` and `status` in your own comments on a PR or MR (see [From your phone](/daily-use#from-your-phone)). Only the token's own user, checked by user id |
| `check_updates` | `true` | once a day, ask GitHub's public releases API for the latest version number; the TUI header and `nightporter status` then say when there's a new one. Anonymous: GitHub sees your IP and a `nightporter/<version>` user agent, nothing about you or your repos. Night Porter never updates itself |
| `[[accounts]]` | | `forge = "github" \| "gitlab" \| "forgejo"` (`"gitea"` works too), `host` (default github.com / gitlab.com / codeberg.org), `token = "cli" \| "keyring"` (`cli` asks `gh`/`glab`/`tea`; store a keyring token with `nightporter token set <forge> [host]`), `aliases` (other host names your clone remotes use, such as an SSH `Host` alias from `~/.ssh/config`: `["github-work"]`; `git.drupal.org` is built in for `git.drupalcode.org`). See [Forgejo and Gitea](#forgejo-and-gitea) |
| `[notifications]` | all `true` | `needs_approval`, `failed`, `done`, `new_author`: the desktop notification for each kind |
| `[[notifications.targets]]` | none | webhooks and commands that also get notifications; see [Notification targets](#notification-targets) |

## Per-repo keys (`[defaults]` or a repo file)

| key | default | description |
|---|---|---|
| `subscribed` | `false` | repo files only |
| `agent` | `claude` | `claude` (Claude Code), `opencode`, `codex` or `gemini` (Gemini CLI 0.67+, experimental) |
| `agent_args` | `[]` | extra CLI args. A list goes to whichever agent runs (`["--model", "opus"]`); a table sets them per agent (see [Models per agent](#models-per-agent)). Empty: each agent uses its own default model and settings |
| `allow_commands` | `[]` | commands the agent may run (prefix match), e.g. `["npm test"]`. Claude Code and Gemini CLI: Codex has no per-command allow list |
| `level` | `draft` | `observe`, `suggest`, `draft`, `auto` |
| `triggers` | all | `review_comment`, `review`, `comment`, `ci_failure`, `merge_conflict` |
| `debounce` | `2m` | quiet time before a batch of events becomes a job |
| `max_concurrent` | unlimited | running jobs for this repo |
| `max_ci_attempts` | `3` | automatic CI-fix attempts before giving up (reset when CI goes green) |
| `ci_auto` | `false` | let CI-failure jobs run at `auto`; by default they stop at `draft`, because CI output is untrusted text |
| `timeout` | `30m` | per agent run and per env command |
| `max_budget_usd` | none | Claude Code only |
| `sandbox` | `true` | OS sandbox for the agent's shell commands. Claude Code: Seatbelt on macOS; needs `bubblewrap` and `socat` on Linux; fails closed. Codex: its own sandbox (`--sandbox workspace-write`, network off; `danger-full-access` when `false`). Gemini CLI: its tool sandbox (Seatbelt on macOS, bubblewrap on Linux; network off). Set `false` for repos whose tests need Docker/DDEV sockets. OpenCode has no sandbox. |
| `sandbox_domains` | `[]` | domains sandboxed commands may reach (Claude Code only; Codex and Gemini CLI jobs have no network) |
| `sandbox_read_allow` | `[]` | extra folders in your home folder that sandboxed commands may read, e.g. `["~/.config/mytool"]`; `["~/"]` lifts the home-folder restriction. The worktree and common toolchain folders (`~/.cargo`, `~/.rustup`, `~/.npm`, `~/.nvm`, `~/go`, `~/.composer`, `~/.cache`, …) are always readable. Claude Code only: Gemini CLI's sandbox reads the worktree, its git dir and the folders on your `PATH`. |
| `prompt` | none | your own extra instructions file (relative to the config dir) |
| `packs` | `[]` | enabled packs |
| `[trust]` | | `roles` (`owner`, `maintainer`, `member`, `collaborator`, `contributor`, `none`), `allow_users`, `deny_users`, `allow_teams` / `deny_teams` (`"org/team"` on GitHub and Forgejo/Gitea, `"group"` on GitLab), `allow_bots` (bots are never trusted by role), `open` |
| `notify` | everywhere | where notifications go: `"desktop"` and/or [target](#notification-targets) names, e.g. `["desktop", "phone"]` |
| `[[schedule]]` | always | `days` (`mon-fri`, `sat,sun`, `weekdays`, `weekends`, `daily`), `windows` (`["08:00-09:00", "22:00-06:00"]`), `tz`, and optionally `level`, `agent`, `notify` for that window; see [Working hours](/advanced#working-hours) |
| `[reply]` | | `style`, `language` (`en`), `footer` (`true`), `resolve_threads` (`false`; Gitea only on Forgejo/Gitea, see below) |
| `[checks]` | | `protected` (extra globs), `escalate_on` (`["deps", "size"]`), `max_diff_lines` (`500`) |
| `[env]` | | `setup`, `teardown`, `cleanup` (commands), `prompt` (file) |
| `[maintainer]` | | `enabled` (`false`), `level` (`draft`, capped at draft), `label` (`nightporter`) |

Arrays in a repo file replace the default, except the trust lists (`allow_users`,
`deny_users`, `allow_teams`, `deny_teams`, `allow_bots`) and `checks.protected`, which combine
with the global ones. A repo file can add trusted people or protected paths but can't remove a
global deny.

## Limits that config cannot change

- `trust.open = true` caps the level at `suggest`.
- Maintainer mode is capped at `draft`.
- A schedule window's `level` is checked like `level`: the caps above still apply, CI-failure jobs
  still stop at `draft` unless `ci_auto`, and a window can lower maintainer jobs but never raise
  them.
- The secrets, protected-path, history and Night Porter-file checks always run. Protected paths
  include CI configs, CODEOWNERS, `.env*`, keys, and the agent instruction files `CLAUDE.md`,
  `AGENTS.md`, `GEMINI.md`, `.claude/`, `.opencode/`, `.codex/`, `.gemini/`, `.cursor/`.
- Night Porter never merges, force-pushes, closes or approves.

## Notification targets

Besides the desktop notification, each event can go to webhooks and commands:

```toml
[[notifications.targets]]
name = "phone"
type = "webhook"
url_env = "NIGHTPORTER_NTFY_URL"   # e.g. https://ntfy.sh/my-secret-topic
format = "ntfy"
kinds = ["needs_approval", "failed"]

[[notifications.targets]]
name = "team"
type = "webhook"
url = "https://hooks.slack.com/services/..."
format = "slack"

[[notifications.targets]]
name = "log"
type = "command"
command = ["/usr/local/bin/np-hook", "--quiet"]
```

| key | description |
|---|---|
| `name` | your name for it, used by `notify` lists and `nightporter notify test <name>`. `desktop` is taken |
| `type` | `webhook` or `command` |
| `url` / `url_env` | webhooks: the URL, or the environment variable that holds it (one of the two). The variable must be set where the daemon runs (launchd and systemd don't read your shell profile) |
| `format` | webhooks: `json` (default), `slack`, `discord` or `ntfy` |
| `command` | commands: the program and its arguments. Run without a shell; the event comes as JSON on stdin |
| `kinds` | only these: `needs_approval`, `failed`, `done`, `new_author`. Default: all |

Every event has `kind`, `title` (e.g. `owner/repo#7`), `summary`, and where they apply `repo`,
`number`, `pr_title`, `url` (the PR or MR) and `job`. `json` webhooks and commands get exactly
that. `slack` and `discord` get a short message with the link; `ntfy` gets the PR title and
summary as the message, a `Title`, `Tags` by kind and a `Click` link to the PR.

Sending is best effort: a target gets 10 seconds, failures are logged, nothing is retried.
Webhook URLs are never logged. `nightporter notify test [name]` sends a sample event (ignoring
`kinds`) and prints what worked.

## Examples

A repo that should only ever suggest:

```toml
# repos/github/acme/payments.toml
subscribed = true
level = "suggest"
[checks]
protected = ["migrations/**", "src/billing/**"]
```

A Drupal module on drupalcode.org with DDEV and maintainer mode:

```toml
# repos/gitlab/project/token.toml
subscribed = true
allow_commands = ["ddev phpunit", "ddev exec phpcs"]
[maintainer]
enabled = true
[env]
setup = ["ddev config --project-type=drupal11 --project-name=np-{repo}-{n} --auto", "ddev start", "ddev composer install"]
teardown = ["ddev stop"]
cleanup = ["ddev delete -O -y"]
prompt = "prompts/drupal.md"
```

## Models per agent

Leave `agent_args` empty and each agent uses whatever it's set up to use: Claude Code runs with
your own user settings, so the model you picked there (`/model`, or `model` in
`~/.claude/settings.json`) applies to jobs too. To pin a model, give each agent its own list:

```toml
# config.toml
[defaults.agent_args]
claude = ["--model", "opus"]
codex = ["-m", "gpt-5"]
opencode = ["--model", "anthropic/claude-sonnet-5-5"]

# repos/gitlab/project/token.toml: only Claude changes here, Codex keeps the default above
[agent_args]
claude = ["--model", "sonnet"]
```

Tables merge key by key, so a repo file only needs the agents it changes. Use a table when a
[schedule window](/advanced#working-hours) switches agents: a plain list would hand Claude's
flags to Codex. `nightporter repo <owner/name>` shows the args that apply.

## Forgejo and Gitea

One integration covers Forgejo and Gitea: codeberg.org, gitea.com and self-hosted servers.

```toml
[[accounts]]
forge = "forgejo"          # or "gitea"
host = "codeberg.org"      # the default; set it for gitea.com or your own server
token = "keyring"          # or "cli" to use your `tea` login for this host
```

- **Token**: `token = "cli"` reads it from [`tea`](https://gitea.com/gitea/tea) (`tea login helper
  get`, which also refreshes OAuth logins). Otherwise create an access token with the scopes
  `read:user`, `read:notification`, `write:issue`, `write:repository` and `read:organization`, and
  store it with `nightporter token set forgejo <host>`. `forgejo-cli` (`fj`) can't hand out its
  token, so it isn't used.
- **What's watched**: notifications (comments, reviews and line comments on your PRs, and
  `/nightporter` commands and the maintainer label on others' issues and PRs), plus the open-PR
  sweep for merge conflicts and CI. CI means commit statuses, which Forgejo/Gitea Actions,
  Woodpecker and most other CI systems report.
- **Roles**: Forgejo and Gitea don't say how a commenter relates to the repo, so Night Porter asks
  for their repo permission (`owner` → owner, `admin` → maintainer, `write` → collaborator, `read` →
  contributor). Only repo admins can see other people's permission; otherwise the repo owner is
  `owner`, collaborators are `collaborator` and everyone else is `none`, including org members
  who aren't collaborators. Trust them with `allow_teams` or `allow_users`.
- **Replies to line comments** are posted as a one-comment review on the same file and line,
  which the web UI shows in the same conversation. If the agent's push moved that line, it may
  show up as a new conversation. Replies to comments on removed lines go to the PR conversation.
- **Resolving threads** (`resolve_threads`) works on Gitea 1.25 and later. Forgejo has no API for
  it, so the job log says so and the conversation stays open.
- **Merge conflicts** fire after two sweeps in a row see one at the same commit, because the
  server also reports "not mergeable" while it is still checking a new push.
- **Draft PRs** (maintainer `fix`) are opened with a `WIP:` title prefix.
