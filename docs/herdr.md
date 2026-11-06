---
description: "Take over a Night Porter job interactively in herdr or Rex, open the queue as a herdr popup, and script Night Porter with events and wait."
---

# Working with herdr and Rex

Night Porter works with [herdr](https://herdr.dev) and
[Rex](https://www.superlogical.com/rex). They’re where you run and watch agents; Night Porter is
what runs while you’re not there.

## Take over a job

On a finished or waiting job, press `h` in the TUI, or run:

```sh
nightporter handoff <id>
```

Night Porter opens the job’s worktree in herdr (a new workspace) or Rex (a new session), starts
your agent and briefs it: what was asked, what it changed, which checks need you, and what is
drafted but not posted. You carry on from there.

Which one it uses:

```toml
# ~/.config/nightporter/config.toml
handoff = "auto"   # or "herdr" or "rex"
```

`auto` uses the one you’re running Night Porter in, else whichever is installed (herdr first).
Rex’s `rex` command lives inside the app on macOS; Night Porter finds it there, or set `REX_BIN`.

## The queue inside herdr

Install the plugin to open Night Porter as a herdr popup:

```sh
herdr plugin install night-porter/nightporter
```

Then bind it to a key in herdr’s config:

```toml
[[keys.command]]
key = "prefix+n"
type = "plugin_action"
command = "nightporter.open-queue"
description = "Night Porter queue"
```

The plugin needs herdr 0.9.0 or newer, the `nightporter` binary on your `PATH`, and the daemon
running for live updates.

## Follow jobs from scripts

```sh
nightporter events --json   # one JSON line per job or inbox change
nightporter wait 42         # exit 0 done, 1 failed or rejected, 2 needs you, 3 timed out
```

Handy for a status bar, a Rex Lua script, or `nightporter wait 42 && say done`.

## Teach your agent about Night Porter

```sh
nightporter skill setup     # installs a Claude Code skill
```

The skill runs `nightporter skill show`, which prints a guide that matches your installed version:
how to read the queue, what the checks mean, and that it should never approve anything unless you
ask. Commands that change anything refuse to run inside a Night Porter job. You can also install
it with `npx skills add night-porter/skills`.
