# Daily use

## What a job is

When a subscribed PR gets activity you’ve enabled (`triggers`), Night Porter waits for it to go
quiet (`debounce`, 2 minutes by default) and turns everything new into one **job**. The agent
runs in its own worktree, Night Porter checks its commits, and then, depending on the level,
the job pushes and replies or waits for you.

## The TUI

Run `nightporter`. It has four tabs:

| key | tab | shows |
|---|---|---|
| `1` | Queue | jobs that need you, running jobs, and failures from the last 24 hours |
| `2` | Inbox | events from people you don’t trust yet |
| `3` | History | recent jobs |
| `4` | Repos | your clones; space toggles the subscription, `R` rescans |
| `?` | Help | keys and commands |

Move with the arrow keys or `j`/`k`, `g`/`G` for the first and last item. `Enter` opens a job and
`Esc` closes it. `p` pauses and resumes. `q` quits. Set `NO_COLOR=1` to turn colours off.

### On a job

| key | does |
|---|---|
| `a` | approve: push and post. On a job waiting for **NEEDS RUN APPROVAL**, let the agent run. On a `suggest` job, mark it done. |
| `A` | approve even though a blocking check failed (asks first) |
| `r` | reject: nothing is pushed or posted (asks first) |
| `t` | retry: run the same requests again as a new job |
| `h` | take over in [herdr](/herdr) |
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
| `d` | never act on this author on this repo (asks first; `nightporter inbox deny <user> --global` for everywhere) |

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
nightporter handoff 42           # take over in herdr

nightporter inbox list
nightporter inbox allow-once <event>
nightporter inbox allow alice --repo acme/api    # or --global
nightporter inbox deny mallory --global
nightporter inbox ignore <event>

nightporter pause                # no new jobs; running ones finish
nightporter resume
nightporter gc                   # remove idle worktrees (--all: every unused one)
```

Setup and maintenance:

```sh
nightporter setup                      # the first-run wizard, again
nightporter doctor                     # check tools, logins, config and disk
nightporter repos                      # clones found, and which are subscribed
nightporter subscribe acme/api         # or host/owner/name; unsubscribe to stop
nightporter service install            # start the daemon at login (uninstall to remove)
nightporter daemon                     # or run it in the foreground
nightporter token set github [host]    # store a personal access token in the OS keychain
nightporter market add <git-url>       # see Packs in Advanced recipes
nightporter pack add <name> | update <name> | remove <name> | list
```

Run `nightporter --help` or `nightporter <command> --help` for the full list.

## Notifications

You get a desktop notification when a job needs approval, fails or finishes, and when a new
author lands in the inbox. Turn any of them off under `[notifications]` in `config.toml`.
