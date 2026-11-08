---
description: "How Night Porter stays safe when acting on text written by strangers: trust, credential isolation, sandboxing and pre-push checks."
---

# Night Porter threat model

Night Porter reads text written by other people (review comments, issues, merge requests) and
runs a coding agent on your machine because of it. This document says who is trusted, what each
security boundary is, and which protections cannot be turned off.

## Assets

1. **Forge credentials**: GitHub/GitLab/Forgejo tokens, and your git SSH keys.
2. **Your repositories**: their history, branches and the code that gets merged.
3. **Your machine**: files, other projects and secrets in your home directory.
4. **Your reputation**: comments and commits posted under your name.

## Actors

| actor | trusted? | how they reach Night Porter |
|---|---|---|
| You | yes | config files, TUI approvals, `/nightporter approve` etc. in your own PR comments |
| Maintainers (role in `trust.roles`, or allow-listed user, team or bot) | for **requests** | comments and reviews on your PRs, `/nightporter` commands, labels |
| Anyone else on the forge | **no** | comments, issues, MRs, fork code |
| Pack authors | no (reviewed by you at install, pinned to a commit) | prompt files |
| The agent (Claude Code / OpenCode / Codex) | **no**; treated as possibly confused or injected | runs in a worktree |

## Boundaries and controls

### B1. Untrusted text never reaches the agent unasked

- Trust rules are checked in this order:
  1. a denied user or team is rejected
  2. a bot is accepted only if it is in `allow_bots`, and is otherwise dropped (bots are never
     trusted by role: anyone with access can install one)
  3. an allowed user or team is accepted
  4. a comment author whose role is in `trust.roles` is accepted
  5. with `trust.open = true`, everyone else is accepted (and the level is capped at `suggest`)
  6. everyone else is untrusted
- GitHub reports `MEMBER`/`COLLABORATOR` for read-only users too, so Night Porter checks the
  author's real repo permission (write or higher counts as collaborator).
- Items from untrusted authors go to the TUI inbox and are not processed until you choose
  allow always, allow once, ignore or deny. "Allow always" on a repo only releases that repo's
  waiting items.
- Global `allow_users` match a username on every configured host. Put host-specific trust in
  repo files if you use several forges.
- Night Porter's own replies carry a hidden marker and are never read back as requests, and
  `/nightporter` in agent-written replies is neutralised.
- When the context for a job is built, comments from untrusted authors are **removed**, not
  quoted.
- Maintainer mode: the *request* must come from a trusted user (a label or a
  `/nightporter <verb>` command). Issue or MR content from others is passed to the agent inside
  clearly marked `UNTRUSTED` blocks, and the job's autonomy is capped at `draft`.
- `trust.open = true` (accept anyone) is capped at `suggest`, so nothing is posted or pushed
  automatically.
- Jobs triggered by failing CI stop at `draft` (unless `ci_auto = true`): CI output is text from
  builds and tools that a PR's code or a third-party check can control.
- Team rules read every page of a team's members. If a `deny_teams` lookup fails, the author goes
  to the inbox instead of being trusted.

### B1a. Job commands in PR comments

`/nightporter approve|dismiss|retry|status` in a PR or MR comment steers Night Porter's newest job
on it (`pr_commands = true` by default; `false` turns it off). Why this is safe to have on:

- Only the token's own user counts, matched by numeric user ID (not login, which can be
  renamed). Trust rules don't apply: maintainers, allow-listed users and bots can't issue them.
  A GitHub comment posted by an app on your behalf (`performed_via_github_app`) doesn't count.
  If the user ID is unknown, nothing counts.
- Anyone who can post as you already holds your forge account (or a token Night Porter itself
  could use), and could push to your branch directly; a comment gives them nothing more.
- Agent-written replies are posted as you, so they are the one way text could come back with
  your user ID: they have `/nightporter` neutralised and carry the hidden marker, and comments
  with the marker are never read as commands.
- The comment must be newer than the job, and each comment ID is recorded before it is acted
  on, so an old or repeated comment never acts again (or on a later job).
- `approve` only releases a job waiting for push approval, with no override for blocking checks;
  approving a run of someone else's code (B4) still needs the TUI, which shows the changed build
  files. B3 checks still run before the push. It never touches the PR itself: there is no merge,
  close or forge approval.
- Command comments, from anyone, are dropped at intake: never a request, never review feedback
  for the agent.

### B2. The agent holds no forge credentials

- The agent process starts with forge tokens and anything that looks like a secret (`*TOKEN*`,
  `*SECRET*`, `*KEY*`, `*AUTH*`, `*PASSWORD*`, `DATABASE_URL`, ...) removed, except the agent's
  own API key. `SSH_AUTH_SOCK` is removed, `GIT_SSH_COMMAND` fails, credential helpers are
  cleared and git hooks are disabled for the agent's commits. `GH_CONFIG_DIR` and `GLAB_CONFIG_DIR` point to empty directories, so `gh` and `glab`
  inside the agent are logged out.
- The generated agent settings deny `git push`, `gh`, `glab`, `tea`, `fj` and network commands, deny
  reading credential files (`~/.ssh`, `~/.config/gh`, `~/.config/tea`, `~/.git-credentials`, ...) and Night
  Porter's own config, state database, log and other jobs' files, and only
  allow git subcommands that can't write outside the repo (`git log/diff/show` take `--output`).
- Claude Code runs with `--setting-sources user`, so a branch's `.claude/settings.json` (hooks,
  permission allows) is ignored. Your own `~/.claude/settings.json` still loads (that's where
  your login comes from), but hooks are switched off for jobs (`disableAllHooks`) and Night
  Porter's denies always win over your allows. Your personal allow rules and plugins do apply:
  keep them narrow. `nightporter doctor` warns when you have any.
- The file tools can't read outside the worktree (`blockReadsOutsideWorkingDirectories`), and
  sandboxed commands can't read your home folder except the worktree, its git dir and common
  toolchain folders (`sandbox_read_allow` adds more). The agent's git uses its own config with
  only your name and email.
- The prompt reaches Claude Code and Codex on stdin, not as a command-line argument (visible in `ps`).
- **Only Night Porter pushes and posts**, and only after the B3 checks. Merge, force-push, close
  and approve are not implemented at all.
- With Claude Code, the agent's shell commands run in Claude Code's OS sandbox by default
  (`sandbox = true`): no network unless you list domains, and the read denies above also apply
  to commands. It fails closed: if the sandbox can't start, the job fails. Verified in headless
  `-p` mode on macOS (2026-11-05): an allowed `cat` of a read-denied path fails with
  "Operation not permitted" (`cargo test real_agent_smoke -- --ignored`).
- With Codex, Night Porter runs `codex exec --sandbox workspace-write` (writes to the worktree
  only, network off; `read-only` for review jobs; `danger-full-access` only with
  `sandbox = false`) with `--ignore-user-config --ignore-rules --ephemeral`, so your
  `~/.codex/config.toml` (MCP servers, hooks, profiles) and execpolicy rules don't load; the login
  still comes from `CODEX_HOME`. Codex has no per-command allow list, so `allow_commands` and
  `sandbox_domains` don't apply.
- Gaps: OpenCode has no sandbox and no read denies (and its prompt is still an argument). Codex's
  sandbox limits writes and network but not reads: its commands can read files in your home
  folder (credentials stay out of its environment, and with the network off it can't send them
  anywhere, but a reply it writes could quote them; draft level shows you every reply first).
  Repos with `sandbox = false` (e.g. for
  Docker/DDEV tests) run `allow_commands` as you. Keep `allow_commands` small there, and treat
  `auto` mode with that in mind.
- The agent can't redirect the daemon's git: the worktree's `.git` pointer is recorded before the
  agent runs and checked right after, even when the run failed, before anything else touches the
  worktree. A changed pointer removes the worktree, and a reused worktree whose `.git` doesn't
  point into your clone is rebuilt. Diffs, checks and pushes run in your clone on fixed SHAs,
  and every daemon git call disables hooks, fsmonitor and the `ext::` transport.
- The agent runs in its own process group, which is killed when the run ends, so nothing it
  started in the background keeps changing the worktree after the checks.
- Night Porter's config and data directories are readable by you only.

### B3. Pre-push checks

Run on every push, in every mode, including `auto`:

| check | can be disabled? |
|---|---|
| secrets in any commit's patch or message, what a merge commit itself adds, and replies and the PR summary (built-in patterns, plus gitleaks if installed) | **no** |
| protected paths touched by any commit, not only the net diff, matched case-insensitively (CI configs for GitHub, GitLab, Forgejo/Gitea Actions, Woodpecker and others, `.git`, `.gitattributes`, agent instruction files for Claude, OpenCode, Codex, Cursor, Copilot and Gemini, git/editor hooks, `.envrc`, `.ddev`, `.devcontainer`, compose files, plus your globs) | **no** (you can only add to the list) |
| force-push or history rewrite | **no** |
| Night Porter files in a commit | **no** |
| dependency manifest or lockfile changes | yes (`escalate_on`), with a warning |
| diff size over the limit | yes (`escalate_on`), with a warning |

A failed check never pushes. It sends the job back to manual approval in the TUI; approving
past a blocking failure needs an explicit override. What is pushed is exactly the reviewed
commit SHA.

### B4. Running other people's code

Any job on a branch you didn't write (any maintainer request on someone else's PR or MR), and any
`reproduce` that runs an environment or allowed commands, needs
your approval **before the agent starts at all**: the branch can carry agent configuration
(CLAUDE.md, `.codex/`, opencode.json, plugins) and build scripts. The approval is bound to the head SHA you
saw; if they push again, Night Porter asks again. The same applies to your own PR when its head
branch lives in someone else's fork. Commits someone else pushed to your own branch are not
detected. `reproduce` jobs that may run commands need
the same approval. The TUI shows
which build files changed (`composer.json`, `package.json`, `Makefile`, CI files) before you
approve. Environment commands only ever come from your own config, never from the repository.

Environment commands run outside the agent's sandbox, and what they do depends on files in the
worktree. So Night Porter copies the files they typically read (`.ddev/`, `.devcontainer/`,
compose files, `Makefile`, `justfile`, `.env`) before the agent runs and puts them back before
teardown and cleanup, and the commands get no model API keys. Scripts your commands call by
name (`composer install`, `npm test`) still run the branch's version: keep environment commands
to repos whose scripts you'd run yourself.

### B5. Packs

Packs contain only TOML, markdown and plain text: no executable code, pinned to a commit SHA. A pack step can
block a job or require approval; it can never relax B1–B4. Updating a pack shows the prompt diff
before it is applied. A step that needs write access shows a warning at install time.

### B6. Credentials at rest

Tokens come from `gh`/`glab`/`tea` or from the OS keychain. They are never stored in config, logs, job
directories or the state database, and they are kept in `secrecy` types in memory.

### B7. Nothing left in your repos

Job files live in `~/.local/share/nightporter/jobs/<id>/`. Worktrees live outside your clones. A
pre-push check rejects any commit containing Night Porter files.

### B8. Network calls of its own

Besides your forges (through `gh`/`glab` or the API) and the git URLs of marketplaces and packs
you add, Night Porter makes one call: with `check_updates = true` (the default), the daemon asks
GitHub's public releases API for the latest version number once a day (`nightporter update` asks
now). It is anonymous: no ID and nothing about you or your repos is sent; GitHub sees your IP
address and the `nightporter/<version>` user agent. Night Porter never downloads or installs an
update itself. Set `check_updates = false` to turn it off.

Notification targets you add (`[[notifications.targets]]`) send each event's kind, repo, PR
number, title and link, job id and a one-line summary to the webhook or command you name;
never diffs, comments or tokens. Webhook URLs are never logged or shown in the TUI; keep them
out of the file with `url_env`. The PR title is untrusted text: Slack messages escape it and
Discord messages can't ping anyone.

## Out of scope

- A trusted maintainer asking for something harmful on purpose. Trust is a decision you make;
  the always-on checks still apply.
- Vulnerabilities in the agent CLIs, git, `gh` or `glab` themselves.
- A machine that is already compromised.

Pack and marketplace sources must be `https://`, `ssh://` or `git@` URLs, and revs can't
look like options.

## Open items before 1.0

- [ ] Independent review of B1–B5 against the implementation
- [ ] Fuzz the trust and context-building code with crafted comments
- [x] Claude Code's sandbox applies in `-p` mode (macOS, real run, 2026-11-05)
- [ ] Same check on Linux (bubblewrap)
- [ ] Sandbox for OpenCode runs (macOS `sandbox-exec`, Linux bubblewrap)
- [x] First independent review (2026-11-05): findings fixed, see git history
