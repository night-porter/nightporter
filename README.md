# Night Porter

**Handles your PRs while you're away.**

Night Porter is like an IRC bouncer for your pull requests. It watches GitHub and GitLab for
review comments, failing CI and merge conflicts on your PRs and MRs, then runs a coding agent
(Claude Code or OpenCode) on your local clone to deal with them, within the rules you set.

Most agent tools are cockpits: you start the agent and watch it. Night Porter is the night
shift. It starts work **by itself** when something happens on your PRs, and it's built to be safe
when that something was written by a stranger.

> Status: early development (0.x). Expect breaking changes.

## How it works

1. **Watch.** A small daemon polls GitHub notifications and your open GitLab MRs (no webhooks, no
   public endpoint).
2. **Filter.** Only repos you subscribed to, only events you enabled, and only from people you
   trust. Comments from strangers wait in an inbox until you decide.
3. **Work.** After a short quiet period, Night Porter batches the new activity on a PR and runs
   your agent in a separate git worktree. Your own checkout is never touched.
4. **Check.** Night Porter checks the agent's commits for secrets, protected files, dependency
   changes and size before anything leaves your machine.
5. **Act, or ask.** Depending on the autonomy level, Night Porter pushes and replies on its own,
   or waits for you in the TUI.

## Autonomy levels

| level | what happens |
|---|---|
| `observe` | notify only |
| `suggest` | agent commits locally; you get copy-paste reply text and a push command, and Night Porter posts nothing |
| `draft` | agent commits; Night Porter pushes and replies after you approve |
| `auto` | Night Porter pushes and replies on its own (pre-push checks still apply) |

Night Porter **never** merges, force-pushes, closes or approves.

## Features

- GitHub (github.com) and GitLab (gitlab.com and self-hosted, for example `git.drupalcode.org`)
- Claude Code and OpenCode
- Trust rules by role, user, team or bot, plus an inbox for first-time authors
- Per-repo settings for triggers, autonomy, agent, schedule, reply style and environment
- Several active windows per day; events outside them are queued
- Reply styles (`concise`, `detailed-tech-lead`, `non-technical`, or your own), with an optional
  AI-disclosure footer
- Environment hooks for projects that need a running stack (DDEV, Docker, …)
- Maintainer mode: triage, reproduce, fix or review issues and MRs from others, on request
- Packs and marketplaces: shareable review steps, reply styles and environment presets
- Leaves no files in your repositories

## Works with herdr

[herdr](https://herdr.dev) is where you run and watch agents; Night Porter is what runs while
you're not there. They fit together:

- **Take over a job in herdr.** On any job, press `h` in the TUI (or run
  `nightporter handoff <id>`). Night Porter opens a herdr workspace at that job's worktree,
  starts your agent, and briefs it: what was asked, what it changed, which checks need you,
  and what is drafted but not posted. You continue from there, interactively.
- **The queue inside herdr.** Install the plugin and open Night Porter as a herdr popup:

  ```sh
  herdr plugin install night-porter/nightporter
  ```

  Then bind it to a key in herdr's config:

  ```toml
  [[keys.command]]
  key = "prefix+n"
  type = "plugin_action"
  command = "nightporter.open-queue"
  description = "Night Porter queue"
  ```

## Install

> Releases are not published yet. Build from source:

```sh
cargo install --path .
nightporter setup      # first-run wizard
nightporter            # open the TUI
nightporter doctor     # check gh / glab / agents / git
```

Requirements: `git`, plus `gh` (GitHub) and/or `glab` (GitLab), plus `claude` and/or `opencode`.
`gitleaks` is optional but recommended. `herdr` is optional, for taking over jobs interactively.

## Configuration

Config lives in `~/.config/nightporter/`: `config.toml` for defaults, and
`repos/<forge>/<owner>/<repo>.toml` for per-repo overrides. Runtime state lives in
`~/.local/share/nightporter/`.

```toml
version = 1
code_dir = "~/coding"

[defaults]
agent = "claude"
level = "draft"
triggers = ["review_comment", "review", "comment", "ci_failure", "merge_conflict"]
debounce = "2m"

[defaults.trust]
roles = ["owner", "maintainer", "member", "collaborator"]
allow_bots = ["coderabbitai[bot]"]

[[defaults.schedule]]
days = "mon-fri"
windows = ["08:00-09:00", "17:00-22:00"]

[defaults.reply]
style = "concise"
footer = true
```

See [`docs/`](docs/) for the full reference.

## Security

Read [`docs/threat-model.md`](docs/threat-model.md). In short: untrusted text never reaches the
agent unless you allow it, the agent never holds forge credentials, and some pre-push checks can't
be turned off. To report a vulnerability, see [SECURITY.md](SECURITY.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your
option.
