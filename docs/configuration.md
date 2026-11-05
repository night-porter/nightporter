# Configuration reference

Files:

| file | purpose |
|---|---|
| `~/.config/nightporter/config.toml` | global settings, accounts and `[defaults]` |
| `~/.config/nightporter/repos/<forge>/<owner>/<repo>.toml` | per-repo overrides (any `[defaults]` key, plus `subscribed`) |
| `~/.config/nightporter/packs.lock` | installed packs, pinned |
| `~/.local/share/nightporter/` | state database, job files, worktrees, packs, daemon log |

`NIGHTPORTER_HOME=/some/dir` moves both to `/some/dir/{config,data}` (handy for testing).
The daemon reloads config files when they change. An invalid file is reported and the previous
config stays active. Unknown keys are errors, so typos don't fail silently.

## Global keys

| key | default | |
|---|---|---|
| `version` | `1` | config format version |
| `code_dir` | `~/coding` | where your clones live |
| `scan_depth` | `3` | how deep to look for clones |
| `poll_interval` | `2m` | never faster than the forge's `X-Poll-Interval` |
| `sweep_interval` | `10m` | open-PR sweep for merge conflicts and CI |
| `min_free_gb` | `5` | jobs wait when free disk is below this |
| `max_concurrent_jobs` | `2` | across all repos |
| `daily_job_cap` | `50` | jobs per 24h |
| `retention_days` | `30` | history kept in the state db |
| `worktree_idle_days` | `14` | idle worktrees are removed after this |
| `marketplaces` | `[]` | extra marketplace git URLs |
| `[[accounts]]` | | `forge = "github" \| "gitlab"`, `host` (default github.com / gitlab.com), `token = "cli" \| "keyring"` |
| `[notifications]` | all `true` | `needs_approval`, `failed`, `done`, `new_author` |

## Per-repo keys (`[defaults]` or a repo file)

| key | default | |
|---|---|---|
| `subscribed` | `false` | repo files only |
| `agent` | `claude` | `claude` or `opencode` |
| `agent_args` | `[]` | extra CLI args, e.g. `["--model", "opus"]` |
| `allow_commands` | `[]` | commands the agent may run (prefix match), e.g. `["npm test"]` |
| `level` | `draft` | `observe`, `suggest`, `draft`, `auto` |
| `triggers` | all | `review_comment`, `review`, `comment`, `ci_failure`, `merge_conflict` |
| `debounce` | `2m` | quiet time before a batch of events becomes a job |
| `max_concurrent` | unlimited | running jobs for this repo |
| `max_ci_attempts` | `3` | automatic CI-fix attempts before giving up (reset when CI goes green) |
| `timeout` | `30m` | per agent run and per env command |
| `max_budget_usd` | none | Claude Code only |
| `sandbox` | `true` | Claude Code's OS sandbox for the agent's shell commands (Seatbelt on macOS; needs `bubblewrap` and `socat` on Linux). Fails closed. Set `false` for repos whose tests need Docker/DDEV sockets. OpenCode has no sandbox. |
| `sandbox_domains` | `[]` | domains sandboxed commands may reach |
| `prompt` | none | your own extra instructions file (relative to the config dir) |
| `packs` | `[]` | enabled packs |
| `[trust]` | | `roles`, `allow_users`, `deny_users`, `allow_teams`, `deny_teams`, `allow_bots`, `open` |
| `[[schedule]]` | always | `days` (`mon-fri`, `sat,sun`, `daily`), `windows` (`["08:00-09:00", "22:00-06:00"]`), `tz` |
| `[reply]` | | `style`, `language`, `footer` (`true`), `resolve_threads` (`false`) |
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
- The secrets, protected-path, history and Night Porter-file checks always run. Protected paths
  include CI configs, CODEOWNERS, `.env*`, keys, and the agent instruction files `CLAUDE.md`,
  `AGENTS.md`, `.claude/`, `.opencode/`.
- Night Porter never merges, force-pushes, closes or approves.

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
