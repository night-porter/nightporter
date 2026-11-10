# Changelog

All notable changes to Night Porter. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) (0.x: expect breaking changes).

## [Unreleased]

### Added

- `nightporter events --json` job lines include `state` (stable status name) and `actions`
  (the commands that apply now), for scripts and agents.
- Review bots in `trust.allow_bots` get their whole review handled (GitHub): a job on your own
  PR also takes the bot's unresolved review threads you haven't answered, read from the PR
  itself. After the push, Night Porter asks Greptile for a new review (`@greptile review`), and
  stops after `max_ci_attempts` rounds per PR.

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

[Unreleased]: https://github.com/night-porter/nightporter/compare/v0.4.3...HEAD
[0.4.3]: https://github.com/night-porter/nightporter/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/night-porter/nightporter/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/night-porter/nightporter/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/night-porter/nightporter/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/night-porter/nightporter/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/night-porter/nightporter/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/night-porter/nightporter/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/night-porter/nightporter/releases/tag/v0.1.0
