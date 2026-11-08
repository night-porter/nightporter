# Changelog

All notable changes to Night Porter. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) (0.x: expect breaking changes).

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

[0.3.0]: https://github.com/night-porter/nightporter/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/night-porter/nightporter/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/night-porter/nightporter/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/night-porter/nightporter/releases/tag/v0.1.0
