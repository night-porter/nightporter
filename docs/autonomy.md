---
description: "How much Night Porter does on its own: levels from observe to auto, per repo and per kind of job, maintainer mode, the caps no setting lifts, and the token scopes each feature needs."
---

# Autonomy and permissions

Night Porter starts careful and gets more freedom only where you give it. This page has every
setting that decides how far a job goes on its own, and what your token must allow.

## Levels

| level | what a job does |
|---|---|
| `observe` | nothing on the forge; it only tells you there's something new |
| `suggest` | does the work and leaves the result for you to copy (`c` copies the push command) |
| `draft` | does the work and waits in the Queue for you to approve it (default) |
| `auto` | pushes and posts by itself when every check is clean; otherwise it waits like `draft` |
| `merge` | `auto`, then turns on the forge's own auto-merge for the PR, where the branch's rules allow it ([below](#merge)) |

Set `level` in `[defaults]` or in a repo's file (`l` on the Repos tab cycles it).

## Per kind of job

Different kinds of work deserve different levels. `levels` sets one per kind of job on your own
PRs, over `level`:

```toml
level = "draft"
levels = { ci_failure = "auto", review_comment = "auto", comment = "draft" }
```

Kinds: `review_comment`, `review`, `comment`, `ci_failure`, `merge_conflict`. A job that batches
several kinds gets the lowest of their levels.

CI output is untrusted text, so CI-failure jobs stop at `draft` unless `ci_failure` is set above
`draft` here (or `ci_auto = true`).

## Maintainer mode

Jobs on other people's issues and PRs (`/nightporter fix`, the `nightporter` label, review
requests, [sprint pickup](/advanced#pick-up-the-sprint)) stop at `draft`. On a project you maintain, unlock them in **that repo's
own file**. It can't be set in `[defaults]`, so it never unlocks every repo at once:

```toml
# repos/github/acme/api.toml
[maintainer]
enabled = true
allow_above_draft = true
levels = { triage = "auto", fix = "draft", review = "draft" }
```

Verbs: `triage`, `reproduce`, `fix`, `review`. A formal review (`review_verdicts`) and running
someone else's code still wait for you at any level.

## Schedule windows

A `[[schedule]]` window with a `level` is the default and the most any kind gets while it's
active, so a quiet daytime window holds everything back and a night window can raise the
default without lifting a kind you set lower. See [Working hours](/advanced#working-hours).

## Caps

Only an explicit opt-in lifts these:

- CI-failure jobs stop at `draft` unless `ci_failure` is set above it (or `ci_auto`).
- Maintainer jobs stop at `draft` unless the repo's own file sets `allow_above_draft`.

No setting lifts these:

- `trust.open = true` caps everything at `suggest`.
- The secrets, personal-data, protected-path and history checks always run; a failed check makes
  an `auto` job wait for you.
- Night Porter never merges by default, never force-pushes someone else's work, and never closes
  anything. `merge` (below) hands merging to the forge, behind its rules.

## Merge

`merge` is `auto` plus one step: after pushing, Night Porter turns on the forge's own auto-merge
(GitHub auto-merge, GitLab "merge when pipeline succeeds", Forgejo "merge when checks succeed").
The forge merges later, when its own rules are met, never before. An issue fix at `merge` opens
its PR ready for review instead of as a draft.

It does this only on a branch whose rules, readable with your token, require all of:

- status checks that must pass,
- at least one approving review,
- a new approval after the latest push (GitHub "require approval of the most recent push",
  GitLab "reset approvals on push" with authors unable to approve their own, Forgejo "dismiss
  stale approvals").

Night Porter never approves its own PR, so the approval comes from someone else. If the rules
don't prove all of that, or can't be read, the job stays at `auto`: pushed, left open, and the job
log says why.

```toml
# repos/github/acme/api.toml
level = "draft"
levels = { ci_failure = "merge", review_comment = "auto" }
```

- GitHub: rulesets are read with normal access. Classic branch protection needs admin access to
  read, so with it alone, add a ruleset or the job stays at `auto`. Turn on "Allow auto-merge" in
  the repo settings.
- GitLab: approval rules are a paid feature; on GitLab Free a job at `merge` stays at `auto`.
- Forgejo/Gitea: branch protection is readable by repo admins only.

`merge` works per kind with `levels`, and for maintainer jobs only where the repo
[opts in](#maintainer-mode). `trust.open` still caps everything at `suggest`, and CI-failure jobs
reach it only with `ci_failure` set above `draft`.

## Token scopes

`nightporter doctor` logs in with each account's token and says which features its scopes don't
cover. A missing optional scope turns off only that feature.

| feature | GitHub (classic or `gh` login) | GitHub fine-grained | GitLab |
|---|---|---|---|
| PRs, issues, comments, pushes | `repo` | Contents, Pull requests, Issues: read and write | `api` |
| notifications (new activity) | `notifications` or `repo` | Notifications aren't available to fine-grained tokens: use `gh auth login` | `api` |
| team trust (`trust.allow_teams`) | `read:org` | Members: read (organization) | `api` |
| sprint pickup (assign, comment) | `repo` | Issues: read and write | `api`, and the Reporter role |
| `merge` (read rules, enable auto-merge) | `repo` | Contents, Pull requests: read and write; Administration: read for classic protection | `api`, and the Developer role |

GitHub fine-grained tokens and GitLab OAuth logins don't list their scopes, so `doctor` names what
they need instead of checking. Forgejo/Gitea tokens need `read:user, read:notification,
write:issue, write:repository, read:organization` (`nightporter token set forgejo <host>`).

Add a missing GitHub scope with `gh auth refresh --hostname github.com --scopes read:org`.
