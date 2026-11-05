# Night Porter threat model

Night Porter reads text written by other people (review comments, issues, merge requests) and
runs a coding agent on your machine because of it. This document says who is trusted, what each
security boundary is, and which protections cannot be turned off.

## Assets

1. **Forge credentials**: GitHub/GitLab tokens, and your git SSH keys.
2. **Your repositories**: their history, branches and the code that gets merged.
3. **Your machine**: files, other projects and secrets in your home directory.
4. **Your reputation**: comments and commits posted under your name.

## Actors

| actor | trusted? | how they reach Night Porter |
|---|---|---|
| You | yes | config files, TUI approvals |
| Maintainers (role in `trust.roles`, or allow-listed user, team or bot) | for **requests** | comments and reviews on your PRs, `/nightporter` commands, labels |
| Anyone else on the forge | **no** | comments, issues, MRs, fork code |
| Pack authors | no (reviewed by you at install, pinned to a commit) | prompt files |
| The agent (Claude Code / OpenCode) | **no**; treated as possibly confused or injected | runs in a worktree |

## Boundaries and controls

### B1. Untrusted text never reaches the agent unasked

- Trust rules are checked in this order:
  1. a denied user or team is rejected
  2. an allowed user, team or bot is accepted
  3. a comment author whose role is in `trust.roles` is accepted
  4. everyone else is untrusted
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

### B2. The agent holds no forge credentials

- The agent process starts with forge tokens and anything that looks like a secret (`*TOKEN*`,
  `*SECRET*`, `*KEY*`, `*AUTH*`, `*PASSWORD*`, `DATABASE_URL`, ...) removed, except the agent's
  own API key. `SSH_AUTH_SOCK` is removed, `GIT_SSH_COMMAND` fails, credential helpers are
  cleared and git hooks are disabled for the agent's commits. `GH_CONFIG_DIR` and `GLAB_CONFIG_DIR` point to empty directories, so `gh` and `glab`
  inside the agent are logged out.
- The generated agent settings deny `git push`, `gh`, `glab` and network commands, deny
  reading credential files (`~/.ssh`, `~/.config/gh`, `~/.git-credentials`, ...), and only
  allow git subcommands that can't write outside the repo (`git log/diff/show` take `--output`).
- Claude Code runs with `--setting-sources user`, so a branch's `.claude/settings.json` (hooks,
  permission allows) is ignored.
- **Only Night Porter pushes and posts**, and only after the B3 checks. Merge, force-push, close
  and approve are not implemented at all.
- Known gap: there is **no OS-level sandbox**. Read denies cover Claude Code's own tools, but
  any command you add to `allow_commands` runs as you and can read what you can. OpenCode has no
  read denies. Keep `allow_commands` small, and treat `auto` mode with that in mind.
- The agent can't redirect the daemon's git: the worktree's `.git` pointer is recorded before the
  agent runs and checked afterwards. Diffs, checks and pushes run in your clone on fixed SHAs,
  and every daemon git call disables hooks, fsmonitor and the `ext::` transport.

### B3. Pre-push checks

Run on every push, in every mode, including `auto`:

| check | can be disabled? |
|---|---|
| secrets in any commit's patch or message, and in replies (built-in patterns, plus gitleaks if installed) | **no** |
| protected paths touched by any commit, not only the net diff (CI configs, `.git`, `.gitattributes`, agent instruction files, plus your globs) | **no** (you can only add to the list) |
| force-push or history rewrite | **no** |
| Night Porter files in a commit | **no** |
| dependency manifest or lockfile changes | yes (`escalate_on`), with a warning |
| diff size over the limit | yes (`escalate_on`), with a warning |

A failed check never pushes. It sends the job back to manual approval in the TUI; approving
past a blocking failure needs an explicit override. What is pushed is exactly the reviewed
commit SHA.

### B4. Running other people's code

Any job on a branch written by an untrusted author (maintainer `review`/`fix` on their MR) needs
your approval **before the agent starts at all**: the branch can carry agent configuration
(CLAUDE.md, opencode.json, plugins) and build scripts. The approval is bound to the head SHA you
saw; if they push again, Night Porter asks again. `reproduce` jobs that may run commands need
the same approval. The TUI shows
which build files changed (`composer.json`, `package.json`, `Makefile`, CI files) before you
approve. Environment commands only ever come from your own config, never from the repository.

### B5. Packs

Packs contain only TOML and markdown: no executable code, pinned to a commit SHA. A pack step can
block a job or require approval; it can never relax B1–B4. Updating a pack shows the prompt diff
before it is applied. A step that needs write access shows a warning at install time.

### B6. Credentials at rest

Tokens come from `gh`/`glab` or from the OS keychain. They are never stored in config, logs, job
directories or the state database, and they are kept in `secrecy` types in memory.

### B7. Nothing left in your repos

Job files live in `~/.local/share/nightporter/jobs/<id>/`. Worktrees live outside your clones. A
pre-push check rejects any commit containing Night Porter files.

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
- [ ] OS-level sandboxing for the agent (Claude Code sandbox settings, macOS `sandbox-exec`,
      Linux bubblewrap)
- [x] First independent review (2026-11-05): findings fixed, see git history
