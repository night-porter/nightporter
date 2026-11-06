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
agent = "claude"
agent_args = ["--model", "opus"]
allow_commands = ["npm test", "npm run lint"]   # prefix match
prompt = "prompts/house-style.md"               # relative to the config dir
```

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

## Replies

```toml
[defaults.reply]
style = "concise"          # detailed-tech-lead, non-technical, or one from a pack
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
