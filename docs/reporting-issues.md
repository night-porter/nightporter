# Reporting issues

## Bugs

[Open a bug report](https://github.com/night-porter/nightporter/issues/new?template=bug_report.md)
and include:

- `nightporter --version` and your OS
- the forge (GitHub, GitLab, self-hosted) and the agent with its version
- what happened, what you expected, and how to reproduce it
- the relevant config and log lines, **with tokens and private repo names removed**

The output of `nightporter doctor` helps too.

## Ideas

[Open a feature request](https://github.com/night-porter/nightporter/issues/new?template=feature_request.md).
New review steps, reply styles and environment presets can often be a
[pack](/writing-packs) instead of a core change.

## Security vulnerabilities

**Don’t open a public issue.**
[Report it privately](https://github.com/night-porter/nightporter/security/advisories/new) on
GitHub. Night Porter runs an agent because of text written by other people, so anything that
lets an untrusted author reach the agent, bypass a pre-push check or see a token counts.

<!--@include: ../SECURITY.md{12,}-->
