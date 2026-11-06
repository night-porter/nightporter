# Getting started

Night Porter watches GitHub and GitLab for activity on your PRs and MRs: review comments, failing
CI and merge conflicts. When something happens, it runs your coding agent (Claude Code or
OpenCode) in a separate git worktree, checks the result, and then pushes and replies, or waits
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
| `claude` or `opencode` | the agent |
| `gitleaks` | optional, a stronger secrets check |
| `herdr` or Rex | optional, for [taking over jobs](/herdr) |
| `bubblewrap` and `socat` | Linux only, for Claude Code’s sandbox |

Log in to `gh` and/or `glab` first (`gh auth login`, `glab auth login`), and run `claude` or
`opencode` once so it’s logged in too.

## 2. Install Night Porter

Releases aren’t published yet. Build it from source with Rust:

```sh
git clone https://github.com/night-porter/nightporter
cd nightporter
cargo install --path .
```

## 3. Run the setup wizard

```sh
nightporter setup
```

It asks for:

1. **Forges**: GitHub, GitLab or both, and the host if it’s self-hosted.
2. **Default agent**: Claude Code or OpenCode.
3. **A tool check**: the same as `nightporter doctor`. Nothing is installed for you; it prints
   the command to fix each problem.
4. **Where your clones live** (`~/coding` by default). Night Porter works on the clones you
   already have.
5. **Default autonomy level**. Start at `suggest` or `draft`: `draft` lets you approve every push,
   `suggest` never pushes at all. See [autonomy levels](#autonomy-levels).
6. **Active hours**, for example `mon-fri 08:00-09:00,17:00-22:00`. Leave it empty to run
   any time.
7. **Repos to subscribe to**, picked from the clones it found.
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
nightporter repos                       # what it found, and what’s subscribed
nightporter subscribe acme/api          # owner/name
nightporter subscribe git.example.com/team/app   # self-hosted host
nightporter unsubscribe acme/api
```

Or press `4` in the TUI and toggle repos with the space bar.
