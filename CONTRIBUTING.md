# Contributing to Night Porter

Thanks for helping. Night Porter is a single Rust binary: a daemon plus a terminal UI.

## Getting started

```sh
git clone https://github.com/dan2k3k4/nightporter
cd nightporter
cargo build
cargo test          # unit + integration tests, no network, no real agent
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The tests never call GitHub, GitLab or a real agent. Forge APIs are replayed from JSON fixtures
in `tests/fixtures/`, and agents are replaced by `tests/stub-agent.sh`, which makes a
predictable commit and writes a `replies.json`.

## Ground rules

- **Security first.** Read `docs/threat-model.md` before changing anything in `trust`, `agent`,
  `checks`, `pipeline` or `packs`. A change that loosens a security boundary needs an issue
  first.
- **Nothing in user repos.** Night Porter must never leave files in a repository or worktree it
  works on.
- **Keep it small.** Prefer the standard library and existing dependencies. New dependencies
  need a reason in the PR.
- **TUI accessibility.** Everything must work with the keyboard alone, colour must never be the
  only signal (use text or symbols too), and every view must also work with `--plain`.
- One topic per PR, with a test for any new logic.
- Commits are signed (`git commit -S`).

## Packs

Want to add a review step, a reply style or an environment preset? That's usually a **pack**,
not a core change. See `docs/writing-packs.md`.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).

## License

Contributions are dual-licensed under MIT OR Apache-2.0, the same as the project, unless you
say otherwise.
