---
description: "Take over a Night Porter job in herdr, Rex, tmux, Zellij or a new terminal, open the queue as a herdr popup, and script it with events and wait."
---

# Working with herdr, Rex, tmux and Zellij

Night Porter works with [herdr](https://herdr.dev),
[Rex](https://www.superlogical.com/rex), tmux and Zellij, or opens a new terminal window. They’re
where you run and watch agents; Night Porter is what runs while you’re not there.

## Take over a job

On a finished or waiting job, press `h` in the TUI, or run:

```sh
nightporter handoff <id>
```

Night Porter opens the job’s worktree, starts your agent and briefs it: what was asked, what it
changed, which checks need you, and what is drafted but not posted. You carry on from there.

Where it opens:

```toml
# ~/.config/nightporter/config.toml
handoff = "auto"   # or "herdr", "rex", "tmux", "zellij", "terminal" or "print"
```

| Value | Opens |
|---|---|
| `herdr` | a new herdr workspace |
| `rex` | a new Rex session |
| `tmux` | a new window when you’re inside tmux, else a detached session (`tmux attach -t …`) |
| `zellij` | a new pane when you’re inside Zellij, else a background session (`zellij attach …`) |
| `terminal` | a new window: iTerm2 or Terminal on macOS; `$TERMINAL`, then `x-terminal-emulator`, Ghostty, kitty, WezTerm, GNOME Terminal or Konsole on Linux |
| `print` | nothing: it prints the `cd … && <agent> …` command for you to run |

`auto` uses the multiplexer you’re running Night Porter in (herdr, Rex, tmux or Zellij), else
herdr or Rex if installed, else a new terminal window (not over SSH), else prints the command.
If opening fails, `auto` prints the command too.

### Choose the agent

By default a handoff starts the agent that ran the job. `handoff_agent` picks another one,
including agents Night Porter doesn't run jobs with:

```toml
handoff_agent = "pi"   # or job, claude, opencode, codex, gemini, cursor, copilot, goose, qwen, droid, kiro
```

| `handoff_agent` | starts |
|---|---|
| `pi` | [pi](https://github.com/badlogic/pi-mono): `pi <briefing>` |
| `cursor` | Cursor CLI: `cursor-agent <briefing>` |
| `copilot` | GitHub Copilot CLI: `copilot -i <briefing>` |
| `goose` | Goose: `goose run -s -t <briefing>` |
| `qwen` | Qwen Code: `qwen -i <briefing>` |
| `droid` | Factory Droid: `droid <briefing>` |
| `kiro` | Kiro CLI: `kiro-cli chat <briefing>` |

These are for taking over only. Unattended jobs need an agent Night Porter can fence in (a
per-command allow list, a sandbox, no repo-level config from an untrusted branch), so jobs still
run with `agent`. `nightporter doctor` checks the handoff agent is installed. In herdr, the agent
is started with `herdr agent start --kind <binary>`, so herdr has to know it.

For tmux, Zellij, terminal and print, the briefing is written to `handoff-brief.md` in the job’s
directory and read from there when the agent starts; it is never pasted into a command line.
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
