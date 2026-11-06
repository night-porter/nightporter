# Working with herdr

Night Porter is compatible with [herdr](https://herdr.dev). herdr is where you run and watch
agents; Night Porter is what runs while you’re not there.

## Take over a job

On any job, press `h` in the TUI, or run:

```sh
nightporter handoff <id>
```

Night Porter opens a herdr workspace at that job’s worktree, starts your agent and briefs it:
what was asked, what it changed, which checks need you, and what is drafted but not posted.
You carry on from there.

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

The plugin needs the `nightporter` binary on your `PATH`, and the daemon running for live
updates.
