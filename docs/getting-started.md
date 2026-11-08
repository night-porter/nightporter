---
description: "Install Night Porter with Homebrew, the install script or Cargo, run the setup wizard, subscribe to a repo and pick an autonomy level."
---

# Getting started

Night Porter watches GitHub, GitLab and Forgejo/Gitea (Codeberg) for activity on your PRs and MRs: review comments, failing
CI and merge conflicts. When something happens, it runs your coding agent (Claude Code,
OpenCode, Codex or Gemini CLI) in a separate git worktree, checks the result, and then pushes and replies, or waits
for you. How far it goes is up to you.

::: warning Early development
Night Porter is 0.x. Expect breaking changes.
:::

## 1. Install the tools it needs

| tool | needed for |
|---|---|
| `git` | everything |
| `gh` | GitHub |
| `glab` | GitLab (gitlab.com or self-hosted) |
| an access token, or `tea` | Forgejo/Gitea (codeberg.org, gitea.com or self-hosted), see below |
| `claude`, `opencode`, `codex` or `gemini` | the agent (Codex: `npm install -g @openai/codex` or `brew install --cask codex`; Gemini CLI 0.67 or newer: `npm install -g @google/gemini-cli`) |
| `gitleaks` | optional, a stronger secrets check |
| herdr, Rex, tmux or Zellij | optional, for [taking over jobs](/herdr) |
| `bubblewrap` and `socat` | Linux only, for Claude Code’s sandbox (Gemini CLI needs `bubblewrap`) |

Log in to `gh` and/or `glab` first (`gh auth login`, `glab auth login`), and run `claude`,
`opencode` or `codex` once so it’s logged in too. Gemini CLI needs a paid Gemini API key
(`GEMINI_API_KEY` in the daemon’s environment) or a Gemini Code Assist Standard/Enterprise login.

**Forgejo and Gitea** (Codeberg, gitea.com, self-hosted) use one integration. Night Porter takes
the token from [`tea`](https://gitea.com/gitea/tea) if you have a `tea` login for the host
(`tea login add --url https://codeberg.org`); `tea` works with Forgejo too. Otherwise the setup
wizard asks you to paste an access token and keeps it in the OS keychain. Create it under
**Settings → Applications** (for Codeberg: <https://codeberg.org/user/settings/applications>)
with these scopes:

| scope | used for |
|---|---|
| `read:user` | who you are |
| `read:notification` | new comments and reviews |
| `write:issue` | reading issues and PR comments, posting replies |
| `write:repository` | PRs, reviews, CI statuses, line-comment replies, opening draft PRs |
| `read:organization` | `allow_teams` / `deny_teams` |

## 2. Install Night Porter

Pick one.

**Homebrew** (macOS and Linux):

```sh
brew install night-porter/tap/nightporter
```

**Install script** (macOS and Linux). It downloads the right binary from GitHub Releases:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/night-porter/nightporter/releases/latest/download/nightporter-installer.sh | sh
```

**Cargo**, if you have Rust:

```sh
cargo install nightporter --locked
```

**Windows** (experimental), in PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/night-porter/nightporter/releases/latest/download/nightporter-installer.ps1 | iex"
```

Or download an archive for your platform from
[GitHub Releases](https://github.com/night-porter/nightporter/releases), check it against its
`.sha256` file, and put `nightporter` on your `PATH`.

**From source:**

```sh
git clone https://github.com/night-porter/nightporter
cd nightporter
cargo install --path . --locked
```

To update later, run the same command again (`brew upgrade nightporter` with Homebrew).
Night Porter never updates itself, but once a day it checks whether there's a new release and
says so in the TUI header ("update available") and in `nightporter status`; `nightporter update`
checks right now. See [`check_updates`](/configuration#global-keys) to turn that off.

## 3. Run the setup wizard

```sh
nightporter setup
```

If you skip this and open the TUI (`nightporter`) first, it offers to run the wizard for you.
You can run it again at any time: `nightporter setup`, or press `S` in the TUI.

In a terminal the wizard uses menus and a searchable checklist: arrow keys move, you can edit an
answer in place, and in the repo list you type to search and press space to tick. Set
`NIGHTPORTER_PLAIN=1` (or use `TERM=dumb`) for plain line-by-line prompts, which work better with
screen readers and in scripts.

It asks for:

1. **Forges**: GitHub, GitLab, Forgejo/Gitea or several, and the host if it’s self-hosted. For
   Forgejo/Gitea it uses your `tea` login, or asks for a token (see above).
2. **Default agent**: Claude Code, OpenCode, Codex or Gemini CLI.
3. **A tool check**: the same as `nightporter doctor`. Nothing is installed for you; it prints
   the command to fix each problem.
4. **Where your clones live** (`~/coding` by default). Night Porter works on the clones you
   already have.
5. **Default autonomy level**. Start at `suggest` or `draft`: `draft` lets you approve every push,
   `suggest` never pushes at all. See [autonomy levels](#autonomy-levels).
6. **Active hours**, for example `mon-fri 08:00-09:00,17:00-22:00`. Leave it empty to run
   any time.
7. **Repos to subscribe to**, ticked in a searchable list of the clones it found. Repos you
   already subscribed to start ticked; untick one to unsubscribe.
8. **The background service**, so the daemon starts at login (launchd on macOS, systemd
   `--user` on Linux).

Everything it writes ends up in `~/.config/nightporter/config.toml`, which you can edit by
hand. See the [configuration reference](/configuration).

## 4. Check everything is ready

```sh
nightporter doctor
```

Every line should be OK. Anything else comes with a `fix:` line. See
[Troubleshooting](/troubleshooting) if one doesn’t go away.

## 5. Open the TUI

```sh
nightporter
```

Jobs show up in the **Queue** tab when your subscribed PRs get activity. Press `?` for help.
[Daily use](/daily-use) explains the rest.

## Autonomy levels

| level | what happens |
|---|---|
| `observe` | notify only |
| `suggest` | the agent commits locally; you get copy-paste reply text and a push command, and Night Porter posts nothing |
| `draft` | the agent commits; Night Porter pushes and replies after you approve |
| `auto` | Night Porter pushes and replies on its own (pre-push checks still apply) |

At any level, Night Porter **never** merges, force-pushes, closes or approves.

## Subscribing to more repos

```sh
nightporter repos                       # subscribed repos, then a count of the other clones
nightporter repos api --all             # search by name; --all lists every clone
nightporter subscribe acme/api          # owner/name
nightporter subscribe git.example.com/team/app   # self-hosted host
nightporter unsubscribe acme/api
```

Or press `4` in the TUI: the space bar (or `s`) toggles a repo, `l` cycles its autonomy level (it
asks before `auto`) and `a` switches its agent (Claude Code → OpenCode → Codex → Gemini CLI). These write the
repo file for you. `Enter` on a repo, or `nightporter repo acme/api`, shows everything that shapes
work on it: level, agent, prompt files, reply style, trust, hours and checks.

## Turning Night Porter off

Night Porter has two parts. The **daemon** is the background part that watches your PRs and
runs jobs. The **TUI** is the window you look at. Closing the TUI (`q`) does **not** stop the
daemon: it keeps watching.

To turn it off, press `D` in the TUI (it asks first), or run:

```sh
nightporter stop
```

Jobs that were running stop too. Nothing half-done is pushed or posted; they're queued again
the next time it starts. Your repos and config are not touched.

To turn it back on, press `D` again (the header says **NOT RUNNING (D starts it)** while it's
off) or run `nightporter start`.

If you installed the background service, it also starts again when you log in. To stop that:

```sh
nightporter service uninstall
```
