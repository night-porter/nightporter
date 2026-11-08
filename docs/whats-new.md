---
description: "What's new in Night Porter 0.3: notifications on your phone, day and night shifts, PR-comment commands, Forgejo/Gitea, Gemini CLI (experimental) and more handoff targets."
---

# What's new in 0.3

The full list is in the [changelog](https://github.com/night-porter/nightporter/blob/main/CHANGELOG.md).

## Day shift, night shift

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

## Approve from your phone

Comment on the PR or MR:

```
/nightporter approve
```

`dismiss`, `retry` and `status` work too, on GitHub, GitLab and Forgejo/Gitea. Only comments
from your own account count (checked by user ID), and `approve` only releases a job that is
waiting for push approval. See [From your phone](/daily-use#from-your-phone).

## Forgejo and Gitea

Codeberg, gitea.com and self-hosted servers, with a token or your `tea` login:

```toml
[[accounts]]
forge = "forgejo"
host = "codeberg.org"
```

Resolving threads works on Gitea 1.25 and later only; Forgejo has no API for it. See
[Forgejo and Gitea](/configuration#forgejo-and-gitea).

## Gemini CLI (experimental)

`agent = "gemini"` runs Gemini CLI 0.67 or newer, sandboxed with the network off. It hasn't
been run against a real Gemini CLI yet, so treat it as experimental and start with `draft`. See
[Gemini CLI](/advanced#gemini-cli). Cursor CLI and Amp were evaluated and aren't supported; the
[threat model](/threat-model) says why.

## Take over in tmux, Zellij or a new terminal

`h` (or `nightporter handoff <id>`) now also opens jobs in tmux, Zellij or a new terminal
window, or prints the command. `handoff = "auto"` picks the one you're in and never fails: with
nothing around, it prints the command. See [Take over a job](/herdr#take-over-a-job).

## Also

- The Repos tab shows each repo's own level and agent (what `l` and `a` change), and
  `(now auto, from window)` while a window overrides them.
- Coming with the release, in their own repositories: first-party packs for Drupal, Rust and
  accessibility review, and the Night Porter skill as a Claude Code plugin and a Gemini CLI
  extension. These docs will link them once they're published; until then,
  `nightporter skill setup` installs the skill for Claude Code.
