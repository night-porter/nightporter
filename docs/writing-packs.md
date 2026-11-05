# Writing packs

A **pack** adds behaviour to Night Porter without changing its code: extra review steps, reply
styles and environment presets. A pack is a git repository containing only text files.

## What a pack can and can't do

| can | can't |
|---|---|
| add instructions to the main agent run (`pre_fix`, `post_fix`) | contain scripts, binaries or symlinks (installation refuses) |
| add read-only **gates** that can drop a job (`filter`) or block/ask before a push (`pre_push`) | skip trust rules, pre-push checks or approvals |
| ship reply styles (`haiku-pirate`, `formal-german`, ...) | read files outside its own directory |
| ship `[env]` presets (DDEV, Docker Compose, ...) | update itself: updates show a diff and need the user's OK |

A pack's prompts still steer an agent that can read and edit the user's code, so treat a pack
like a dependency. Users install packs pinned to a commit, and see what they contain first.

## Layout

```
my-pack/
  pack.toml
  steps/a11y.md
  styles/haiku-pirate.md
  envs/drupal-ddev.toml
  README.md
```

Only `.md`, `.toml` and `.txt` files (plus `LICENSE*`, `README`, `NOTICE`) are allowed, and
none of them may be executable.

## `pack.toml`

```toml
name = "a11y"                     # [A-Za-z0-9_-]
description = "Accessibility review for web front-ends"
version = "0.1.0"

[[steps]]
name = "review"
hook = "post_fix"                 # filter | pre_fix | post_fix | pre_push
prompt = "steps/a11y.md"

[[steps]]
name = "spam-gate"
hook = "filter"
prompt = "steps/spam.md"

[[styles]]
name = "haiku-pirate"
file = "styles/haiku-pirate.md"

[[envs]]
name = "drupal-ddev"
file = "envs/drupal-ddev.toml"
```

### Hooks

- **`pre_fix` / `post_fix`**: the prompt text is added to the main agent's task under
  "Before you start" or "After the main task". Keep it short and specific.
- **`filter`**: runs before any work, read-only. End the prompt by asking for a verdict;
  `block` drops the job (shown in history with your summary).
- **`pre_push`**: runs after the agent, read-only, and sees the diff. `needs_approval` sends the
  job to the user even in `auto` mode; `block` marks it as a blocking failure.

Gates must end their answer with:

````
```nightporter
{"verdict": "pass" | "block" | "needs_approval", "summary": "why"}
```
````

### Reply styles

A style is a short markdown file describing tone and length. Users select it with
`reply.style = "haiku-pirate"` (or `"my-pack/haiku-pirate"` if two packs use the same name).

### Env presets

An env preset is a TOML snippet for a repo file's `[env]` table. Users print it with
`nightporter pack env my-pack/drupal-ddev` and paste it into their own config, so commands
never run straight from a pack.

```toml
[env]
setup = ["ddev config --project-name=np-{repo}-{n} --auto", "ddev start", "ddev composer install"]
teardown = ["ddev stop"]
cleanup = ["ddev delete -O -y"]
prompt = "~/.config/nightporter/prompts/drupal.md"
```

Templates: `{repo}` (made safe for names), `{n}`, `{forge}`, `{wt}` (worktree path), `{branch}`.

## Publishing

1. Push the pack to a public git repo.
2. Add it to a marketplace's `marketplace.toml` with a **commit SHA**:

```toml
name = "my-market"
description = "Packs for Drupal maintainers"

[[packs]]
name = "a11y"
url = "https://github.com/you/nightporter-a11y"
rev = "3f2c…full sha…"
description = "Accessibility review for web front-ends"
```

Users add your marketplace with `nightporter market add <git-url>`, then
`nightporter pack add a11y`. To get into the official marketplace, open a PR on
`nightporter-dev/marketplace`.
