# Changelog

All notable changes to Night Porter. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) (0.x: expect breaking changes).

## [Unreleased]

## [0.8.0] - 2026-11-24

### Added

- Every push to a PR says how to undo it: the first reply after it (or a short note when there is none) ends with the `git revert` command that takes the job's commits back.
- `/nightporter stop` holds a PR (a queued or running job stops, new events are dropped, pending ones wait), `/nightporter resume` lets it go again, and `/nightporter do <what>` queues a job for your own words. Your account only, like the other PR commands.
- `nightporter ship` pushes the current branch, opens a draft PR/MR (`--ready`, `--base`, `--title`) and subscribes the repo, so Night Porter looks after it from then on.
- `nightporter stats` and the repo view suggest the next level for a kind of job once its last 10 jobs on your PRs went out exactly as written at `suggest` or `draft`.
- `checks.hold`: path globs whose changes always wait for your approval, at any level (a non-blocking "held paths" check). Repo files add to the global list.
- `maintainer.review_depth`: `quick` (blockers only), `standard`, or `deep` (reads around each change and tries to disprove every finding before reporting it).
- `max_monthly_usd`: a repo's jobs wait once they cost that much in a calendar month; `nightporter run` still starts one.

### Changed

- The TUI's lists are easier to scan: Queue and History rows always show when each job last changed (`o` still switches the order), the Inbox shows when each comment was written, Worktrees and the Inbox are coloured like the job lists, and keys in the footer and Help tab stand out from what they do. `NO_COLOR` turns it all off; the text is the same either way.

## [0.7.0] - 2026-11-23

### Added

- Jira Cloud issues, through `acli` and its logins: `[[jira]]` in a repo's own file maps a site and JQL to the repo (`paths` for the part of the repo, `priority` when several repos match). `pickup = "assigned"` works on issues assigned to you and never changes the assignee; `pickup = "new"` claims new unassigned issues (optionally only from `creators`) by assigning them to you. Jobs open `ABC-123: …` PRs on `nightporter/jira/<site>/ABC-123` branches, comment the PR link and replies on the issue, move it to In Progress (draft PR) and In Review (ready PR), never Done, and let go when the PR merges or closes. Status names are tried from built-in defaults, `[jira_sites."<site>"]` or the entry, and the one that works is remembered per project. `doctor` checks `acli`'s logins, each entry's JQL and overlapping entries. See [Jira issues](docs/advanced.md#jira-issues).
- `o` in the Queue and History tabs orders jobs by creation or by last update; each row shows the date the list is ordered by. `nightporter status` shows when each job was last updated.

### Changed

- CI-fix jobs on GitHub get the end of each failed Actions job's log (up to three jobs, the 60 lines before the last error), read once per pushed commit, so the agent sees the actual errors instead of only the check names.

### Fixed

- Times in the TUI and `nightporter status` are in your local time zone, not UTC.
- PR command polling skips Jira jobs, which have no PR of their own number to read.

## [0.6.1] - 2026-11-17

### Changed

- The agent can read history: before each job the upstream's branches and tags are mirrored under `refs/nightporter/upstream/` (your own tags are untouched), and it may run `git for-each-ref`, `git cat-file`, `git merge-base` and `git ls-tree`, so a triage can say which release has a fix.
- Replies no longer talk about the agent's own limits (tools, permissions, what it couldn't access). A check it didn't run is noted only as "Not verified locally."; the reason stays in the job log.

## [0.6.0] - 2026-11-17

### Added

- The Queue shows what each running job is doing: a spinner, the stage (`worktree`, `gate`, `env`, `plan`, `fix`, `checks`, `push`, …) and how long it has been in it, e.g. `⠹ checks 4m12s`.
- `nightporter run <id>` (TUI `f`) runs a job now: a queued job goes next, past its repo's concurrency; any other is retried and starts past pause, schedule windows, the daily cap and debounce. The one-job-per-PR rule, the disk check and `max_concurrent_jobs` still hold.
- `nightporter cancel <id>` (TUI `K`) stops a job. A running agent is killed (its whole process group), and the job fails through its normal cleanup: env files restored, teardown run. Between agent runs it stops at its next step, and never pushes.
- `nightporter burst <duration> [--jobs N] [--concurrency N]` gives more capacity for a while: no daily cap or sweep reserve, schedule windows ignored, twice the jobs at once (overall and per repo). The TUI header counts it down; `burst off` ends it.
- `levels`: a level per kind of job on your own PRs (`ci_failure`, `review_comment`, `comment`, …) over `level`, e.g. CI fixes at `auto`, comments at `draft`. A job of several kinds gets the lowest; a schedule window's level caps them while it's active.
- `nightporter doctor` checks each account's token scopes and names the features a missing scope turns off, with the command to add it. The new [Autonomy and permissions](docs/autonomy.md) page has levels, caps and scopes in one place.
- `[maintainer.pickup]` claims unassigned issues of the current sprint (`label`, or `milestone`, `"@current"` = the open one due soonest), assigns them to you with a comment, and works on them, at most `max_claimed` (2) at a time. A failed, rejected or dismissed job unassigns the issue again and it isn't claimed twice. `sweep` gets `--milestone` and `--unassigned`.
- `level = "merge"` (also per kind in `levels`): `auto`, then the forge's own auto-merge is turned on for the PR, but only where the target branch's rules, readable with your token, require passing checks, an approval, and a new approval after the latest push. Otherwise the job stays at `auto` and its log says why. Issue fixes at `merge` open their PR ready for review. See [Merge](docs/autonomy.md#merge).
- Sprint pickup from a GitHub Project: `project = "owner/number"`, `iteration = "@current"` (or a title). `doctor` checks for `read:project`.
- `maintainer.triage_new` and `maintainer.review_new` (off by default): triage every new issue and review every new ready PR/MR by others, from when they're turned on.
- `maintainer.levels` (per verb: `triage`, `fix`, …) and `maintainer.allow_above_draft` (in a repo's own file only) let maintainer jobs on that repo go above `draft`.

### Changed

- `retry` (TUI `t`, CLI) also takes a job waiting for you, rejecting it first; `/nightporter retry` on a PR still doesn't. An issue job's branch is now rebuilt from the base and force-pushed over the head the last run left, with a lease; if anyone else pushed to it since, the job continues the branch instead. `--keep` always continues it. `--now` starts it like `run`.

## [0.5.0] - 2026-11-13

### Added

- **Plan first**: `plan_first = ["fix", …]` per repo. Those jobs first write a plan in a
  read-only run and wait for you (even at `auto`); `a` builds it, `e` edits the plan first. The
  build follows the approved plan. Plans stay local.
- **Learn from your fixes**: `L` on the Repos tab or `nightporter learn <repo>` starts a job that
  reads your edits, amends, rejections and dismissals on that repo (trusted jobs only, no CI output) and
  proposes a better `prompt` instructions file, shown as a diff for you to approve. A reminder
  comes once a repo has five unlearned fixes, at most weekly.
- **`nightporter stats`**: jobs, cost, cost per pushed job, and how often you rejected, edited,
  amended, took over or retried, by repo, kind of job and agent/model (`--repo`, `--days`). The
  repo view shows the last 30 days.
- `trigger_agent_args` takes `plan` and `learn` keys, for a model per step.
- `w` in the TUI opens the job's PR/MR or issue in your browser (Queue, History, job view), the
  comment (Inbox) or the repo (Repos). The job view and `nightporter show` print the link.

## [0.4.6] - 2026-11-12

### Added

- **Edit before it goes out**: `e` in the TUI (or `nightporter edit <id>`) opens a waiting job's
  replies in `$EDITOR`; the footer/marker go back on and the reply checks run again. `m` (or
  `nightporter amend <id>`) picks up your own changes in the job's worktree: committed, signed
  and checked again like the agent's, no agent run, then back to you for approval.

- **No-trace replies**: `reply.attribution` per repo: `footer` (default: the visible "via Night
  Porter" line plus the hidden `<!-- nightporter -->` marker), `marker` (hidden only, same as
  `footer = false`) or `none` (neither; command error replies drop the "Night Porter:" prefix).
  For repos where auditors read the page source. New posts only.
- **Models per kind of job**: `trigger_agent_args` sets `agent_args` per trigger
  (`ci_failure`, `merge_conflict`, ...) or maintainer verb (`triage`, ...), e.g. a cheaper model
  for CI fixes. Used when the whole job is that kind, for the main agent run only (pack gates
  keep `agent_args`).
- **Personal-data check**: Luhn-valid card numbers and mod-97-valid IBANs in replies or the PR
  body send the job to approval. Well-known test numbers are allowed. Always on.

### Changed

- CI-fix retries stop early when CI fails exactly the same way as before the last fix (same
  checks, same reported output), instead of using up `max_ci_attempts`. Failures that report
  only job names (GitLab, bare GitHub Actions) still go by `max_ci_attempts`.
- `/nightporter` is neutralised in command status and error replies too, not only agent replies.
- `/nightporter status` replies `Job 12: ...` (was `Night Porter job 12: ...`); an issue fix's
  draft PR with an empty summary is titled `Fix #N` (was `Fix #N: Night Porter fix`).

## [0.4.5] - 2026-11-11

### Added

- **Review verdicts**: with `review_verdicts = ["request_changes", "approve"]` (any of
  `comment`, `request_changes`, `approve`) under `[maintainer]`, a review job's reply is posted
  as a real review with the agent's outcome instead of a plain comment. It always waits for
  your approval, even at `auto`; the job shows the outcome before you approve. Never on your
  own PR. GitLab: approve = note + approval; request changes = note (no API for it).

- **Watch all PRs**: with `watch_prs = true` under `[maintainer]`, every ready (non-draft)
  PR/MR by someone else in a subscribed repo lands in the inbox, polled every `sweep_interval`,
  for PRs nobody assigned you. Allow once (`o`) queues a review at maintainer level; ignore
  (`i`) drops it. Drafts show up once they're marked ready. The first look at a repo fills the
  inbox quietly; after that each new PR notifies (`new_author`).

### Changed

- **Concise style spaces out multi-point replies**: a review or list of notes now leads with the
  verdict, then one `-` bullet per note, instead of one dense paragraph. Simple replies stay one
  to three sentences.

## [0.4.4] - 2026-11-11

### Added

- `nightporter events --json` job lines include `state` (stable status name) and `actions`
  (the commands that apply now), for scripts and agents.
- Review bots in `trust.allow_bots` get their whole review handled (GitHub): a job on your own
  PR also takes the bot's unresolved review threads you haven't answered, read from the PR
  itself. After the push, Night Porter asks Greptile for a new review (`@greptile review`), and
  stops after `max_ci_attempts` rounds per PR.
- Signed commits: when your git config has `commit.gpgsign = true`, Night Porter signs the
  agent's commits with your own git signing setup before they are checked and shown, so the SHA
  you approve is signed. The agent still never sees your keys. If signing fails, the job waits
  for you with a failed `signing` check.
- `env.install = true`: install dependencies from the lockfiles in the repo root before the
  agent starts (`composer install`, `npm ci`, `pnpm install --frozen-lockfile`,
  `yarn install --frozen-lockfile`, `cargo fetch --locked`). Runs outside the sandbox, like
  `env.setup`; never for a read-only review of someone else's code.
- `allow_commands = ["*"]` (Claude Code): the agent may run any command. The deny list still
  applies, and with the sandbox on every command stays inside it.
- The agent is told to verify its work the way the project's CI does (tests, linters, static
  analysis) and to name the exact command and error when something can't run.

### Fixed

- Rejecting a queued job just as the daemon started it no longer marks a running job
  rejected; a TUI and the daemon racing on one job now get "changed state; reload" instead of
  overwriting each other. Allowing or ignoring an inbox event that was already handled (for
  example from a second TUI) is refused instead of running it again, and an error after a job
  settled no longer turns it into `failed`.
- A job started by a failing review-score check (Greptile's confidence gate) only saw the
  check's title, not the review comments behind the score, and changed nothing.

## [0.4.3] - 2026-11-10

### Added

- `checks.allow_env_variants = true` (per repo, off by default): jobs may edit `.env.*` files such
  as `.env.example`, so a review nit on a template no longer has to wait for you. A bare `.env`
  stays protected and the secrets scan still runs.

### Fixed

- Approving a `suggest`-level job said "marked done", which read as if it had pushed and posted.
  It now says nothing went out and how to get the diff and replies.

## [0.4.2] - 2026-11-09

### Added

- `nightporter rescan <repo> [--since 1d]`: re-read reviews and comments on your open PRs/MRs in a
  repo and take in any that were missed. Already-seen ones are skipped.

### Fixed

- GitHub and Forgejo/Gitea: a review or comment whose notification arrived late could be missed
  for good. Each open-PR sweep now also re-reads reviews and comments on your open PRs since the
  previous sweep.
- The agent is told never to report a test, linter or build as passing unless it ran it in that
  job. A read-only review had said "Ruff is clean" without being able to run Ruff.

## [0.4.1] - 2026-11-09

### Added

- **Review requests** (GitHub): with `review_requests = true` under `[maintainer]`, a request for
  your review on a PR queues a `review` job. It runs once per PR head while your review is still
  requested, and waits for you like any maintainer job (`level = "suggest"` keeps it private).

### Changed

- `review` jobs on someone else's PR no longer wait for run approval with Claude Code or Gemini
  CLI: they only read the code (no `allow_commands`, no environment). `review_runs_code = true`
  brings back tests, after you approve the run. Codex and OpenCode reviews still wait.

### Security

- Environment `cleanup` no longer runs on the worktree of someone else's code you never approved
  running (for example a job dismissed while it waited for that approval).

## [0.4.0] - 2026-11-09

### Added

- **Backlog sweep**: `nightporter sweep <repo>` queues a maintainer job for each open issue (or,
  with `--prs`, each open PR/MR by someone else), oldest first. Filter with `--label` (repeat for
  several, all must match), `--older-than 90d`, `--newer-than 14d` and `--limit`; `--dry-run`
  lists without queuing. `--verb` picks `triage` (default), `reproduce`, `fix` or `review`
  (default with `--prs`). Jobs stop at maintainer mode's level (at most draft). Your own PRs go
  first and sweeps never use the last quarter of `daily_job_cap`. Re-running a sweep doesn't
  queue anything twice. GitHub, GitLab and Forgejo/Gitea.
- **Triage checks whether an issue is already done**: if the code already does what it asks, the
  reply says so with evidence and recommends closing it.
- **More agents for handoffs**: `handoff_agent = "pi"`, `"cursor"`, `"copilot"`, `"goose"`,
  `"qwen"`, `"droid"` or `"kiro"` (or `"job"`, the default: the agent that ran the job). These are
  handoff-only; jobs still run with `agent`. `nightporter doctor` checks the one you pick.
- **Per-agent `agent_args`**: a table (`claude = ["--model", "opus"]`, `codex = [...]`) so each
  agent gets its own model and flags, merged key by key with repo files. A plain list still works.
- **Worktrees tab** (`6`) and `nightporter worktrees [rm|restore <job>]`: every worktree Night
  Porter made, removable one by one. Removing (also by `gc` and the automatic cleanup) keeps the
  worktree's commits and uncommitted changes in its clone for 7 days, so `u` / `restore` brings
  it back. Ignored files aren't kept. A new job on the PR also keeps what it would reset (your
  edits after a handoff, or an earlier job's unpushed commits); restoring swaps it back in, and
  what was there is kept too.
- **Host aliases** on `[[accounts]]`: `aliases = ["github-work"]` for SSH `Host` aliases or mirror
  names in clone remotes.

### Security

- The agent could run `git stash show --output=<file>` to write a git config into the clone, whose
  commands Night Porter's own git calls would then run outside the sandbox. Only `git stash push`
  and `pop` are allowed now, and the clone's git config is checked after every agent run: a
  change is undone and fails the job. Found in our own audit; we know of no use in the wild.
- Forgejo/Gitea: when you're not a repo admin, a collaborator with read-only access counted as
  `collaborator` (trusted by default). They're `contributor` now.
- Team and group member lists for `allow_teams` / `deny_teams` fail closed when they can't be
  read to the end (too many pages, or a next-page link to another server).
- GitHub notification paging only follows next-page links back to the API it's configured for.
- `/nightporter approve|dismiss|retry|status` ignores edited comments: people with write access
  can edit yours.
- Taking over a job on someone else's code is refused until you've approved running it.
- `pack update` refuses a pack that renamed itself (it would have replaced another installed
  pack), `pack add` refuses a name already installed from another URL, and pack descriptions
  and diffs are printed without terminal control characters.
- Two marketplace URLs could share a local folder. Marketplaces are cloned again on the next
  `nightporter market update`.
- Discord notifications escape Markdown, so a PR title can't render as a link.
- Handoff briefings (sent to herdr and Rex as typed text) have terminal control characters
  removed, so comment or CI text can't break out of the paste.
- Your own comments are matched by user id as well as login, so someone who takes over a login
  you renamed away from isn't treated as you.
- A reply style from a pack only applies in repos where that pack is enabled.

### Fixed

- Clones whose remote uses `git.drupal.org` (the old name of git.drupalcode.org) weren't found,
  so the repo couldn't be subscribed. The alias is now built in.
- **Security**: GitLab group lookups for `allow_teams` / `deny_teams` only read the first 100
  members, so a member of a larger group named in `deny_teams` wasn't denied. Every page is read
  now, as on GitHub and Forgejo/Gitea.
- `gc` no longer stops at the first worktree it can't remove.
- The setup wizard dropped your working hours when `config.toml` had no `[defaults]` table.
- GitLab: when listing your open MRs failed during the very first sweep, the next sweep could
  start jobs for MRs that were already red or conflicted.
- API errors are matched by their HTTP status instead of by the error text, which includes the
  server's response body.

## [0.3.0] - 2026-11-08

### Added

- **Notification targets**: `[[notifications.targets]]` sends events to webhooks (`slack`,
  `discord`, `ntfy`, `json`) and commands (event as JSON on stdin), optionally only some `kinds`.
  Webhook URLs can come from `url_env` and are never logged. A per-repo `notify` list picks
  `desktop` and/or targets. `nightporter notify test [target]` sends a sample event.
- Events carry the PR link, PR title and job id. On Linux, clicking the desktop notification
  opens the PR (macOS can't yet).
- **Schedule windows can set `level`, `agent` and `notify`** while they're open (for example
  draft by day, auto overnight). The safety caps still apply afterwards.
- **Control jobs from PR comments**: `/nightporter approve`, `dismiss`, `retry` and `status` on a
  PR or MR (GitHub, GitLab, Forgejo/Gitea). Only the token's own user counts, matched by user
  id; command comments never reach the agent. `approve` only releases a job waiting for push
  approval. On by default; `pr_commands = false` turns it off.
- **Forgejo/Gitea forge** (`forge = "forgejo"` or `"gitea"`): codeberg.org, gitea.com and
  self-hosted. Notifications, the open-PR sweep (conflicts, commit-status CI), roles from repo
  permission, tokens from the keychain or `tea`. Resolving threads works on Gitea 1.25+ only.
- **Gemini CLI agent** (`agent = "gemini"`, 0.67 or newer), **experimental**: sandboxed with
  the network off and an admin policy, refused on older versions and on branches carrying
  `.gemini/settings.json` or `.gemini/.env`. Not yet run against a real Gemini CLI.
- **Handoff targets**: `handoff = "tmux"`, `"zellij"`, `"terminal"` (a new window) or `"print"`,
  besides herdr and Rex. `auto` never fails: it falls back to printing the command.
- This changelog, and a [What's new](https://docs.night-porter.com/whats-new) docs page.

### Changed

- The Repos tab shows each repo's own level and agent (what `l` and `a` change), with
  `(now auto, from window)` while a schedule window overrides them.
- The agent can't use `tea`/`fj` or Forgejo/Gitea credentials; `.forgejo/`, `.gitea/` and
  Woodpecker configs are protected paths.

### Not supported

- Cursor CLI and Amp were evaluated and aren't supported: neither can be limited to Night
  Porter's settings for an unattended run. See the [threat model](docs/threat-model.md).

## [0.2.0] - 2026-11-06

### Added

- Codex as a third agent (`agent = "codex"`), in its own sandbox.
- Dismiss a job without replying or pushing (`x`), undo (`u`), delete for good (`X`); CLI
  `dismiss`, `restore`, `purge`. Undo an inbox ignore; search History.
- `nightporter start` / `stop`, and `D` in the TUI.
- A daily anonymous check for a new release (`check_updates`), `nightporter update`. Never
  updates itself.
- `nightporter repo <name>` and `Enter` on the Repos tab: everything that shapes a repo's jobs.
- Interactive setup wizard with a searchable repo checklist; `/` searches the TUI's lists.

## [0.1.1] - 2026-11-06

### Added

- Settings tab in the TUI, the setup wizard from the TUI, and config edits that are validated
  before they're saved.
- Install with Homebrew, the install script or `cargo install`.

## [0.1.0] - 2026-11-06

First release.

- A daemon and a terminal UI that watch GitHub and GitLab (including self-hosted) for review
  comments, failing CI and merge conflicts on your PRs and MRs, and run Claude Code or OpenCode
  in a separate git worktree.
- Autonomy levels `observe`, `suggest`, `draft`, `auto`; never merges, force-pushes, closes or
  approves.
- Trust rules by role, user, team or bot, and an inbox for anyone not yet trusted.
- Pre-push checks for secrets, protected files, dependency changes and size; an OS sandbox for
  Claude Code's commands, on by default.
- Per-repo settings, schedule windows, reply styles, environment hooks, maintainer mode, packs
  and marketplaces.
- Handoff to herdr or Rex, a herdr plugin, `events` and `wait` for scripts, and an agent skill.

[Unreleased]: https://github.com/night-porter/nightporter/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/night-porter/nightporter/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/night-porter/nightporter/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/night-porter/nightporter/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/night-porter/nightporter/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/night-porter/nightporter/compare/v0.4.6...v0.5.0
[0.4.6]: https://github.com/night-porter/nightporter/compare/v0.4.5...v0.4.6
[0.4.5]: https://github.com/night-porter/nightporter/compare/v0.4.4...v0.4.5
[0.4.4]: https://github.com/night-porter/nightporter/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/night-porter/nightporter/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/night-porter/nightporter/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/night-porter/nightporter/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/night-porter/nightporter/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/night-porter/nightporter/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/night-porter/nightporter/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/night-porter/nightporter/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/night-porter/nightporter/releases/tag/v0.1.0
