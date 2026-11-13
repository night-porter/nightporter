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
| `4` | Repos | subscribed repos first, `[x]` when subscribed; `Enter` shows [everything about one repo](#one-repo-at-a-glance), space (or `s`) toggles the subscription, `l` cycles the level, `a` switches the agent (Claude Code → OpenCode → Codex → Gemini CLI, experimental), `e` edits the repo file, `R` rescans. `l` and `a` change the repo’s own setting; when a [schedule window](/advanced#working-hours) overrides it right now, the row says so, e.g. `(now auto, from window)` |
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
| `e` | edit the replies in your `$EDITOR` before they're posted (see below) |
| `m` | amend: pick up your own changes in the job's worktree (see below) |
| `h` | take over in [herdr, Rex, tmux or Zellij](/herdr) |
| `c` | copy the push command (for `suggest` jobs) |
| `y` | copy the reply text |
| `w` | open the PR/MR or issue in your browser (the job view shows the link too) |

### Fix it up before it goes out

Close but not quite? You don't need to take over to change a word or a line.

- **`e` edits the replies.** All of the job's replies open in one file in `$VISUAL` /
  `$EDITOR`, each under a `<!-- reply N: ... -->` line. Change the text, empty a reply to drop
  it, save and quit. The footer and marker go back on, and the secrets and personal-data checks
  run again on what you wrote. Works on jobs waiting for approval and on `suggest` jobs.
- **`m` amends the code.** Change files in the job's worktree, by hand or in a take-over
  (`h`), then press `m`. The daemon commits your changes on top ("Edits before push"), signs
  the commits, runs every pre-push check and pack gate again, and the job waits for you once
  more. No agent runs. Then `a` pushes the lot.

Night Porter keeps what the agent first wrote and what you changed. That's what
[learning from your fixes](#learn-from-your-fixes) reads.

### Plan first

For bigger work, have the agent say what it will do before it does it. List the kinds of job in
the repo's file (or `[defaults]`):

```toml
plan_first = ["fix"]          # issue fixes; also review_comment, ci_failure, triage, …
```

Such a job first runs read-only and writes a plan. It waits in the Queue marked `PLAN:`, even at
`auto`, and nothing is posted. `a` approves the plan and the build runs with it; `e` edits the
plan first; `r` rejects the job. After the plan, the job follows the repo's level as usual.
Plans stay on your machine, so approve them in the TUI or with `nightporter approve`: a
`/nightporter approve` comment on the forge doesn't approve a plan. A job that batches several
kinds of event plans if any of them is listed. Stats count the plan and build runs' cost together.
`trigger_agent_args` can give the plan step its own model
(`plan = ["--model", "opus"]`).

### Learn from your fixes

Each edit (`e`), amend (`m`), rejection and dismissal tells Night Porter something the agent got
wrong. `L` on the Repos tab (or `nightporter learn <owner/name>`) starts a learn job for that
repo: the agent reads those fixes and proposes a better instructions file for the repo, your
`prompt` file.

- It reads only fixes no approved learn has read yet (the latest 20), and only jobs that started
  from people you trust: nothing let in from the inbox, nothing on someone else's code or issue,
  and no CI-failure jobs (CI logs are third-party text).
- It runs with no repository and no commands, just the fixes and the current file.
- The proposal waits in the Queue as a diff. `a` writes the file, `e` edits it first, `r` drops
  it. It writes the repo's own `prompt` file. If the repo has none, or shares its file with
  `[defaults]` or another repo, it makes one under `prompts/` in the config folder (starting from
  the shared text, which it never changes) and points the repo at it. The secrets and
  personal-data checks run on it, since it goes into every future prompt.
- If the file or the repo's `prompt` setting changes before you approve, approving fails rather
  than overwrite your change: learn again for a fresh proposal.

Once a repo has five fixes that haven't been learned from, Night Porter reminds you, at most once
a week per repo. The repo's view (`Enter` on the Repos tab) shows the count too.

### What it costs, and how often you fix it

```sh
nightporter stats                    # last 30 days, every repo
nightporter stats --repo acme/api --days 90
```

Jobs, cost, cost per pushed job, and how often you rejected, dismissed, edited, amended, took
over or retried, by repo, by kind of job and by agent and model. Use it to pick a cheaper model
for a kind of job (`trigger_agent_args`), or to spot a repo that needs better instructions.

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
and checks, plus its last 30 days (jobs, fixes, cost). `e` there edits the repo's file, and `L`
learns from your fixes.

## From your phone

Away from the TUI, comment on the PR or MR (in the GitHub, GitLab or Forgejo/Gitea app or web page, say):

| comment | does |
|---|---|
| `/nightporter approve` | push and post a job that is **NEEDS APPROVAL** (like `a`) |
| `/nightporter dismiss` | put the job away (like `x`) |
| `/nightporter retry` | run a finished or failed job's requests again (like `t`) |
| `/nightporter status` | Night Porter replies with the job's status |

The command acts on Night Porter's newest job for that PR, and only when the comment is newer
than the job. Night Porter adds a 👍 when it's done, or replies with the reason when it couldn't
be (for example, nothing is waiting for approval). Each comment is acted on once.

Only comments from your own account count: the user the token belongs to, checked by user ID.
Comments by collaborators, maintainers, bots or apps posting for you are ignored, and no command
comment ever reaches the agent as feedback. `approve` from a comment is stricter than `a`: it
can't approve past a failed blocking check (`A`), and it can't let the agent run someone else's
code (**NEEDS RUN APPROVAL**); use the TUI for those. "Approve" is always Night Porter's job,
not the PR: Night Porter never merges or closes a PR. If the job is a review with an outcome
from `review_verdicts`, approving the job posts that review, an approval included. Night Porter looks at
PRs whose newest job is waiting for you or changed in the last three days, once per
`poll_interval`. Set `pr_commands = false` to turn it off.

## The CLI

Everything in the TUI also works from the command line, which is handy for scripts and screen
readers.

```sh
nightporter status               # plain-text summary of what needs you
nightporter show 42              # a job: checks, replies, diff, log
nightporter approve 42
nightporter approve 42 --override-blocking
nightporter reject 42
nightporter edit 42              # edit the replies (or a waiting plan or learn proposal) in $EDITOR
nightporter amend 42             # commit and re-check your edits in the job's worktree
nightporter retry 42
nightporter dismiss 42 43        # put away, nothing pushed or posted
nightporter restore 42           # bring a dismissed job back
nightporter purge 42 43          # delete for good, with logs (asks; --yes to skip)
nightporter handoff 42           # take over in herdr, Rex, tmux or Zellij
nightporter wait 42              # until done (0), failed (1), needs you (2) or timeout (3)
nightporter events --json        # follow every job and inbox change
nightporter stats                # cost and how often you fixed jobs (--repo, --days)
nightporter learn acme/api       # propose better instructions from your fixes

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
nightporter worktrees            # every worktree Night Porter made, and removed ones
nightporter worktrees rm 42      # remove job 42's worktree (kept for 7 days)
nightporter worktrees restore 42 # bring it back, uncommitted changes and all
```

Each `events --json` job line carries `state`, a stable name (`queued`, `running`,
`awaiting_approval`, `approved`, `ready`, `awaiting_run_approval`, `done`, `failed`, `rejected`,
`dismissed`), and `actions`, the commands above that apply to it right now. Script against those,
not the human `status` label.

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
