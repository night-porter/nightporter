---
description: "Using Night Porter day to day: the TUI tabs, approving, rejecting and dismissing jobs, the inbox for untrusted authors, settings, the CLI, and turning it off."
---

# Daily use

## What a job is

When a subscribed PR gets activity you’ve enabled (`triggers`), Night Porter waits for it to go
quiet (`debounce`, 2 minutes by default) and turns everything new into one **job**. The agent
runs in its own worktree, Night Porter checks its commits, and then, depending on the level,
the job pushes and replies or waits for you.

## The TUI

Run `nightporter`. It has five tabs:

| key | tab | shows |
|---|---|---|
| `1` | Queue | jobs that need you, running jobs, and failures from the last 24 hours |
| `2` | Inbox | events from people you don’t trust yet |
| `3` | History | every job, newest first, including done and dismissed ones; `/` searches (e.g. `dismissed`, a repo name) |
| `4` | Repos | subscribed repos first, `[x]` when subscribed; `Enter` shows [everything about one repo](#one-repo-at-a-glance), space (or `s`) toggles the subscription, `l` cycles the level, `a` switches the agent (Claude Code → OpenCode → Codex), `e` edits the repo file, `R` rescans |
| `5` | Settings | every `config.toml` setting, its value and whether it's `[set]` or `[default]` |
| `?` | Help | keys and commands |

Move with the arrow keys or `j`/`k`, `g`/`G` for the first and last item. `Enter` opens a job and
`Esc` closes it. `PgUp`/`PgDn` move a page. `/` searches the Repos, Settings and History lists (`Esc`
clears the search). `p` pauses and resumes. `S` runs the setup wizard again. `D` starts the
daemon, or stops it after asking (see [Turning Night Porter off](/getting-started#turning-night-porter-off)). `q`
quits the TUI; the daemon keeps running.

The header counts are coloured by urgency, and the header says **update available** when there's
a newer release. In Settings, on is green and off is red, `[set]` and `[default]` look different,
and global keys are coloured differently from `defaults.*` keys. Every state is also written in
words, so nothing depends on colour; set `NO_COLOR=1` to turn colours off.

### Settings

`Enter` on a setting switches it (on/off and fixed choices) or lets you type a new value; `d` puts it
back to the default. Durations look like `2m`, lists are comma-separated, and active hours look like
`mon-fri 08:00-09:00,17:00-22:00; sat 10:00-12:00`. Changes that loosen a safety default (level
`auto`, `sandbox = false`, `ci_auto`, `trust.open`) ask first.

`e` opens `config.toml` (or, on the Repos tab, the selected repo's file) in `$VISUAL` / `$EDITOR`.
Every change is checked before it's saved: an invalid value is refused with the reason, and an
invalid file from the editor is never written; you can fix it or throw the edit away. If
`config.toml` was broken by hand, the Settings tab says why and `e` fixes it.

### On a job

| key | does |
|---|---|
| `a` | approve: push and post. On a job waiting for **NEEDS RUN APPROVAL**, let the agent run. On a `suggest` job, mark it done. |
| `A` | approve even though a blocking check failed (asks first) |
| `r` | reject: nothing is pushed or posted (asks first) |
| `t` | retry: run the same requests again as a new job |
| `x` | dismiss: put the job away without replying or pushing. Its status becomes `dismissed` and it stays in History |
| `u` | undo the last dismiss, or restore the selected dismissed job |
| `X` | delete a finished or dismissed job for good, with its logs (asks first) |
| `h` | take over in [herdr or Rex](/herdr) |
| `c` | copy the push command (for `suggest` jobs) |
| `y` | copy the reply text |

### In the inbox

Comments from people who aren’t trusted never reach the agent. They wait here.

| key | does |
|---|---|
| `o` | let this one event through; the author stays untrusted |
| `a` | trust the author on this repo |
| `A` | trust the author on every repo (asks first) |
| `i` | ignore this event |
| `u` | undo an ignore |
| `d` | never act on this author on this repo (asks first; `nightporter inbox deny <user> --global` for everywhere) |

### Dismissed and finished jobs

Dismissing (`x`) is for jobs you don't want to act on: nothing is pushed or posted, and the job
moves to History with the status `dismissed`. Done and dismissed jobs are revisited in the
**History** tab: press `/` and type `dismissed` or a repo name. `u` brings a dismissed job back to
where it was. `X` deletes a job and its logs for good; it asks first and can't be undone.

### One repo at a glance

`Enter` on the Repos tab, or `nightporter repo <owner/name>`, shows everything that shapes work
on that repo: which keys its repo file sets, the level, agent and its arguments, triggers, reply
style (with the style text), language and footer, the repo's own `CLAUDE.md` / `AGENTS.md`, your
`prompt` and `env.prompt` files (with their contents), packs, hours, limits, trust lists, sandbox
and checks. `e` there edits the repo's file.

## The CLI

Everything in the TUI also works from the command line, which is handy for scripts and screen
readers.

```sh
nightporter status               # plain-text summary of what needs you
nightporter show 42              # a job: checks, replies, diff, log
nightporter approve 42
nightporter approve 42 --override-blocking
nightporter reject 42
nightporter retry 42
nightporter dismiss 42 43        # put away, nothing pushed or posted
nightporter restore 42           # bring a dismissed job back
nightporter purge 42 43          # delete for good, with logs (asks; --yes to skip)
nightporter handoff 42           # take over in herdr or Rex
nightporter wait 42              # until done (0), failed (1), needs you (2) or timeout (3)
nightporter events --json        # follow every job and inbox change

nightporter inbox list
nightporter inbox allow-once <event>
nightporter inbox allow alice --repo acme/api    # or --global
nightporter inbox deny mallory --global
nightporter inbox ignore <event>
nightporter inbox list --ignored # what you ignored
nightporter inbox restore <event>

nightporter pause                # no new jobs; running ones finish
nightporter resume
nightporter gc                   # remove idle worktrees (--all: every unused one)
```

Setup and maintenance:

```sh
nightporter setup                      # the first-run wizard, again
nightporter doctor                     # check tools, logins, config and disk
nightporter repos [word] [--all]       # subscribed repos (paged; --all: every clone)
nightporter repo acme/api              # everything that shapes work on one repo
nightporter subscribe acme/api         # or host/owner/name; unsubscribe to stop
nightporter start | stop               # turn the background daemon on or off
nightporter service install            # start the daemon at login (uninstall to remove)
nightporter daemon                     # or run it in the foreground
nightporter update                     # check for a newer release now
nightporter token set github [host]    # store a personal access token in the OS keychain
nightporter market add <git-url>       # see Packs in Advanced recipes
nightporter pack add <name> | update <name> | remove <name> | list
```

Run `nightporter --help` or `nightporter <command> --help` for the full list.

## Notifications

You get a desktop notification when a job needs approval, fails or finishes, and when a new
author lands in the inbox. Turn any of them off under `[notifications]` in `config.toml`. On
Linux, clicking the notification opens the PR in your browser. macOS notifications can't do
that (the click opens the app that shows them).

To get them on your phone or in a chat too, add webhooks (Slack, Discord, ntfy, plain JSON) or a
command under `[[notifications.targets]]`; see
[Notification targets](/configuration#notification-targets). Check them with:

```sh
nightporter notify test          # every target, and the desktop
nightporter notify test phone    # one target
```

## Updates

Once a day the daemon asks GitHub for the latest release number (`check_updates = true` by
default). When there's a newer one, the TUI header says **update available** and
`nightporter status` prints the command to upgrade. `nightporter update` checks right away.
Night Porter never updates itself: upgrade with `brew upgrade nightporter`, or
`cargo install nightporter --locked`, or run the install script again.

## Turning Night Porter off

`D` in the TUI or `nightporter stop`; `nightporter start` turns it on again. Closing the TUI
doesn't stop the daemon. See [Turning Night Porter off](/getting-started#turning-night-porter-off).
