# Security Policy

Night Porter runs a coding agent on your machine in response to text written by other people.
Security bugs here can mean code execution, leaked credentials or unwanted pushes, so we take
reports seriously.

## Reporting a vulnerability

**Do not open a public issue.** Use GitHub's private vulnerability reporting:
**Security → Report a vulnerability** on this repository, or email
[security@night-porter.com](mailto:security@night-porter.com).

Please include:

- the version (`nightporter --version`) and OS
- the config (with tokens and private repo names removed) and the trigger (comment, issue, MR)
- what happened, and what you expected the trust rules or pre-push checks to prevent

We aim to acknowledge reports within 7 days and to agree on a disclosure date with you.

## Scope

In scope, with the most severe first:

1. Anything that lets an **untrusted author** (see `docs/threat-model.md`) get text, commands or
   code to the agent, or get Night Porter to push, post or run code without approval.
2. Bypasses of the always-on pre-push checks (secrets, protected paths, force-push, Night Porter
   files in commits).
3. Credential exposure: tokens reaching the agent process, logs, job files or the SQLite state.
4. Packs or marketplaces that can relax checks, run code or escape their pinned commit.
5. Config or state corruption that silently raises autonomy.

Out of scope: problems in Claude Code, OpenCode, `gh`, `glab` or git themselves (report those
upstream), and a trusted maintainer deliberately asking for something harmful. The threat model
explains where that line is.

## Supported versions

Only the latest release gets security fixes until 1.0.
