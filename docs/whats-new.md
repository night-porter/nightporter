---
description: "What's new in Night Porter 0.8, 0.7, 0.6, 0.5, 0.4 and 0.3: steering from the PR, ship a branch, earned promotions, Jira issues, release-aware triage, the merge level, levels per kind of job, sprint pickup, run now and burst, plan first, learning from your fixes, real PR reviews and backlog sweeps."
---

# What's new

The full list is in the [changelog](https://github.com/night-porter/nightporter/blob/main/CHANGELOG.md).

## 0.8

### Warm worktrees and T3 Code (0.8.1)

**Dependencies without the reinstall.** `env.share = ["vendor", "node_modules"]` clones those
folders from your own clone into each new worktree, copy-on-write, so installs only update what
changed and the copies take no disk space until a job edits them. See
[Share dependency folders](/configuration#share-dependency-folders).

**Take over in T3 Code.** `handoff = "t3"` opens the job's worktree in T3 Code with the briefing
on your clipboard. Worktrees of closed issues are now cleaned up too, once their PR is done.

### Steer, ship and promote (0.8.0)

**Steer it from the PR.** `/nightporter stop` stops work on a PR until `/nightporter resume`, and
`/nightporter do <what>` asks for something specific. Every push says how to undo it.

**Ship a branch and walk away.** `nightporter ship` pushes your branch, opens a draft PR and
subscribes the repo, so CI failures, conflicts and reviews on it are handled from then on.

**Promotions it has earned.** After 10 jobs of a kind in a row that you approved unchanged,
`nightporter stats` suggests the next level. `checks.hold` keeps chosen paths (`src/auth/**`)
waiting for you at any level. See [Autonomy and permissions](/autonomy#paths-that-always-wait-for-you).

**Reviews and spend.** `maintainer.review_depth = "deep"` double-checks every finding before it's
posted; `quick` sticks to what blocks a merge. `max_monthly_usd` caps what a repo's jobs cost per month.

**Easier to scan.** Every list in the TUI shows when things last changed and uses colour for
status, level, dates and keys (same text with `NO_COLOR`).

## 0.7

### Jira issues (0.7.0)

**It works on Jira Cloud issues.** Map a Jira project to a repo with `[[jira]]` and Night Porter
picks up the issues assigned to you (or new ones, if you let it), opens a PR named after the
issue key, comments the link, and moves the issue to In Progress and then In Review. It uses
`acli`'s logins, so several sites work side by side. See [Jira issues](/advanced#jira-issues).

**CI fixes see the errors.** On GitHub, a CI-fix job now gets the end of each failed Actions
job's log, so the agent starts from the real compiler or test output instead of a check name.

**Order the queue your way.** `o` in the Queue and History tabs switches between newest created
and last updated, and times are in your own time zone.

## 0.6

### Release-aware triage (0.6.1)

**It checks which release has the fix.** Before each job Night Porter fetches the upstream's
branches and tags into the clone (under `refs/nightporter/upstream/`, so your own tags are left
alone), and the agent can now read that history. A triage can say "fixed in `abc1234`, first
released in v2.0.3" instead of guessing.

**Replies talk about the code, not the agent.** No more "I couldn't read the git history" or
"I wasn't able to run the tests here". A check the agent didn't run is marked
"Not verified locally.", and the reason is in the job log.

### From draft to merge (0.6.0)

Night Porter starts as an intern that only drafts, and you promote it where it has earned it:
per repo, per kind of job, all the way to merging behind your own branch rules. Everything about
levels, caps and token scopes is now on one page, [Autonomy and permissions](/autonomy).

**From draft to merge.** The new `merge` level pushes like `auto`, then hands the PR to your forge's own auto-merge, but only where the branch's rules require green checks and a fresh approval from someone else. Night Porter never approves its own PRs, so a person always signs off. See [Merge](/autonomy#merge).

**A level per kind of job.** Let CI fixes and review replies go out on their own while issue fixes wait for you: `levels = { ci_failure = "auto", review_comment = "auto", comment = "draft" }`. On a project you maintain, `maintainer.allow_above_draft = true` in that repo's file, with `maintainer.levels = { triage = "auto", fix = "draft" }`, lets maintainer jobs go above `draft` too. See [Autonomy and permissions](/autonomy).

**It picks up its share of the sprint.** With `[maintainer.pickup]`, Night Porter claims unassigned issues in the current milestone (or with a sprint label), assigns them to you with a comment, and works on them, two at a time by default. If a job fails or you reject it, the issue is handed back. See [Pick up the sprint](/advanced#pick-up-the-sprint).

**Projects and new arrivals.** Pickup also reads a GitHub Project's current iteration (`project = "acme/5"`, `iteration = "@current"`). `triage_new` and `review_new` triage every new issue and review every new PR from others, from the moment you turn them on. See [Triage and review what's new](/advanced#triage-and-review-what-s-new).

**Run it again, now, or not at all.** Changed a prompt, model or config? `t` (or `nightporter retry`) re-runs a job with what you have now, even one waiting for you; an issue's branch is rebuilt from scratch unless you pass `--keep`. `f` (`nightporter run`) starts a job now, past pause, schedule windows and the daily cap. `K` (`nightporter cancel`) stops a running one. Got budget to burn? `nightporter burst 3h` lifts the daily cap, ignores schedule windows and doubles how many jobs run at once until it runs out. See [Daily use](/daily-use).

**See what a running job is doing.** The Queue shows a spinner, the job's stage (`worktree`, `gate`, `env`, `plan`, `fix`, `checks`, `push`, …) and how long it has been in it, e.g. `⠹ checks 4m12s`.

**Doctor checks your token.** `nightporter doctor` now says which features your token's scopes don't cover, and how to add them.

## 0.5

### Plan first, learn from your fixes, stats (0.5.0)

**Plan first.** For bigger work, have the agent say what it will do before it does it:

```toml
plan_first = ["fix"]          # issue fixes; also review_comment, ci_failure, triage, …
```

Such a job first runs read-only and writes a plan. It waits in the Queue marked `PLAN:`, even at
`auto`, and nothing is posted. `a` builds it, `e` edits the plan first, `r` rejects the job.
Plans stay on your machine. See [Plan first](/daily-use#plan-first).

**Learn from your fixes.** Every edit, amend, rejection and dismissal says something the agent
got wrong. Press `L` on the Repos tab (or run `nightporter learn <owner/name>`) and a job reads
those fixes and proposes a better instructions file for that repo, as a diff you approve. It
reads only jobs that started from people you trust, runs with no repository and no commands,
and changes nothing until you approve. Once a repo has five fixes to learn from, Night Porter
reminds you, at most weekly. See [Learn from your fixes](/daily-use#learn-from-your-fixes).

**What it costs, and how often you fix it.** `nightporter stats` shows jobs, cost, cost per
pushed job, and how often you rejected, edited, amended, took over or retried, by repo, kind of
job and agent/model. The repo view shows the last 30 days.

**Open it in the browser.** `w` opens the job's PR/MR or issue, the inbox comment, or the repo.

## 0.4

### Edit before it goes out, no-trace replies, cheaper models (0.4.6)

**Fix it up before it goes out.** Press `e` on a job waiting for you and its replies open in
your editor: change a word, drop one, save. Press `m` after changing files in the job's
worktree (by hand or after `h`), and Night Porter commits, signs and checks your edits like the
agent's, then waits for you again. No take-over needed for a one-line fix. See
[Fix it up before it goes out](/daily-use#fix-it-up-before-it-goes-out).

**Footer, marker, or neither.** Every reply carries a hidden `<!-- nightporter -->` marker, and
by default a visible "via Night Porter" line too. Where a team doesn't want either in the page
source (auditors read the PRs, say), set it per repo:

```toml
# repos/github/acme/billing.toml
[reply]
attribution = "none"   # or "marker" (hidden only), "footer" (the default)
```

Comments posted before the change keep their marker. If a project asks for AI help to be
disclosed, keep the footer there.

**A model per kind of job.** A lint failure rarely needs the biggest model:

```toml
[defaults.trigger_agent_args]
ci_failure = ["--model", "haiku"]
triage = ["--model", "haiku"]
```

It applies when the whole job is that kind; a mixed batch keeps `agent_args`. See
[Models per kind of job](/configuration#models-per-kind-of-job).

**CI that fails the same way stops sooner.** If CI fails with exactly the same check output as
before the last fix, Night Porter stops retrying and tells you, instead of using up
`max_ci_attempts`. It needs checks that report output (GitHub check runs with a summary, such as
most linters); when the forge only gives job names, `max_ci_attempts` decides as before.

**Card numbers and IBANs in replies wait for you.** Replies are posted as you, so a real card
number (Luhn-valid) or IBAN copied from a log or fixture holds the job for approval. The usual
test numbers are fine.

### Every ready PR, real reviews (0.4.5)

**Watch every ready PR.** `watch_prs = true` under `[maintainer]` puts each ready (not draft)
PR/MR by someone else in the inbox, not just the ones where your review was requested. Press
`o` to queue a review, `i` to skip it.

**Approve or request changes.** With `review_verdicts`, a review job is posted as a real
review with the agent's outcome instead of a comment. It always waits for you to approve it.

```toml
[maintainer]
enabled = true
watch_prs = true
review_verdicts = ["request_changes", "approve"]
```

See [Approve or request changes](/advanced#approve-or-request-changes) and
[Watch every ready PR](/advanced#watch-every-ready-pr).

### Review bots, tests that actually run, signed commits (0.4.4)

**The whole bot review, not one comment.** With a review bot in `allow_bots`, a job on your own
PR now reads the PR's review threads and takes the bot's unresolved ones, including comments
from before you allowed it. A failing "review score" check (Greptile's confidence gate) used to
start a job that saw only the check's title. After the push, Night Porter asks Greptile to
review again, and stops after three rounds per PR. GitHub only for now.

```toml
[defaults.trust]
allow_bots = ["greptile-apps[bot]"]
```

**Dependencies installed, checks run.** `install = true` under `[env]` installs from your
lockfiles before the agent starts (composer, npm, pnpm, yarn, cargo), and
`allow_commands = ["*"]` lets the agent run any command inside the sandbox. It then runs the
tests, linters and static analysis that cover its change, the way your CI does.

**Signed commits.** If your git config signs commits, Night Porter signs the agent's commits with
your own key before you review them. The agent never sees the key.

See [Review bots](/advanced#review-bots), [Dependencies and tests](/advanced#dependencies-and-tests)
and [Signed commits](/advanced#signed-commits).

### Let jobs fix `.env.example` on your own repos (0.4.3)

`.env.*` files are protected, so a review nit on `.env.example` used to wait for you. On repos
you maintain, opt in per repo:

```toml
[checks]
allow_env_variants = true
```

Jobs can then edit `.env.example`, `.env.dist` and other `.env.*` files. A bare `.env` stays
protected and the secrets scan still checks every added line. Also new: approving a
`suggest`-level job now says nothing was pushed or posted, instead of just "marked done".

### Reviews when someone asks for yours (0.4.1)

When someone requests your review on a GitHub PR, Night Porter can prepare one: a summary,
issues with file and line, security, tests and a verdict, waiting for you in the TUI. Turn it on
per repo with `review_requests = true` under `[maintainer]`. With Claude Code or Gemini CLI a
review only reads the code (no tests, no environment), so it starts without waiting for you to
approve running someone else's code. See [Review requests](/advanced#review-requests).

### Sweep the backlog

A module with three hundred open issues, half of them fixed years ago? `nightporter sweep`
queues a maintainer job for each one, oldest first, and triage now starts by checking whether
the code already does what the issue asks. You get a reply to approve that says "already done,
here's where" so you can close it.

```sh
nightporter sweep project/token --older-than 90d --dry-run
nightporter sweep project/token --older-than 90d
nightporter sweep project/token --prs                # review open MRs from others
```

Filter by `--label`, `--older-than`, `--newer-than` and `--limit`. Every job waits for your
approval, your own PRs still go first, and a sweep never takes the last quarter of the daily
cap. See [Work through the backlog](/advanced#work-through-the-backlog).

### See and clean up worktrees

A **Worktrees** tab (`6`) lists every worktree Night Porter made. `x` removes one, and `u`
brings it back for 7 days, with its commits and uncommitted changes, so cleaning up after a
handoff can't cost you an afternoon's work. A new job on the PR keeps your edits before it
resets the worktree, too. From the CLI: `nightporter worktrees`, `rm <job>`,
`restore <job>`. `gc` and the automatic cleanup keep removed work the same way.

### Hand off to pi, Cursor, Copilot and more

`handoff_agent = "pi"` (or `cursor`, `copilot`, `goose`, `qwen`, `droid`, `kiro`) opens a job in
that agent when you take it over. See [Choose the agent](/herdr#choose-the-agent).

### A model per agent

```toml
[defaults.agent_args]
claude = ["--model", "opus"]
codex = ["-m", "gpt-5"]
```

Leave it out and each agent uses its own default. See
[Models per agent](/configuration#models-per-agent).

### git.drupal.org clones

Clones with a `git.drupal.org` remote are found now. For SSH host aliases, add
`aliases = ["github-work"]` to the account.

## 0.3

### Day shift, night shift

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

### Approve from your phone

Comment on the PR or MR:

```
/nightporter approve
```

`dismiss`, `retry` and `status` work too, on GitHub, GitLab and Forgejo/Gitea. Only comments
from your own account count (checked by user ID), and `approve` only releases a job that is
waiting for push approval. See [From your phone](/daily-use#from-your-phone).

### Forgejo and Gitea

Codeberg, gitea.com and self-hosted servers, with a token or your `tea` login:

```toml
[[accounts]]
forge = "forgejo"
host = "codeberg.org"
```

Resolving threads works on Gitea 1.25 and later only; Forgejo has no API for it. See
[Forgejo and Gitea](/configuration#forgejo-and-gitea).

### Gemini CLI (experimental)

`agent = "gemini"` runs Gemini CLI 0.67 or newer, sandboxed with the network off. It hasn't
been run against a real Gemini CLI yet, so treat it as experimental and start with `draft`. See
[Gemini CLI](/advanced#gemini-cli). Cursor CLI and Amp were evaluated and aren't supported; the
[threat model](/threat-model) says why.

### Take over in tmux, Zellij or a new terminal

`h` (or `nightporter handoff <id>`) now also opens jobs in tmux, Zellij or a new terminal
window, or prints the command. `handoff = "auto"` picks the one you're in and never fails: with
nothing around, it prints the command. See [Take over a job](/herdr#take-over-a-job).

### Also

- The Repos tab shows each repo's own level and agent (what `l` and `a` change), and
  `(now auto, from window)` while a window overrides them.
- Coming with the release, in their own repositories: first-party packs for Drupal, Rust and
  accessibility review, and the Night Porter skill as a Claude Code plugin and a Gemini CLI
  extension. These docs will link them once they're published; until then,
  `nightporter skill setup` installs the skill for Claude Code.
