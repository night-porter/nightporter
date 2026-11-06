---
description: "How to report a Night Porter bug or a security vulnerability, and what to include."
---

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
GitHub, or email [security@night-porter.com](mailto:security@night-porter.com). Night Porter runs an agent because of text written by other people, so anything that
lets an untrusted author reach the agent, bypass a pre-push check or see a token counts.

<!--@include: ../SECURITY.md#details-->

## Anything else

Email [hi@night-porter.com](mailto:hi@night-porter.com), or find us on [X](https://x.com/NightPorterDev),
[Bluesky](https://bsky.app/profile/did:plc:zojgcz6b634zmhrkl7hjlozs) and [Mastodon](https://mastodon.social/@nightporter).
