# Contributing to Night Porter

Thanks for helping. Night Porter is a single Rust binary: a daemon plus a terminal UI.

## Getting started

```sh
git clone https://github.com/night-porter/nightporter
cd nightporter
cargo build
cargo test          # unit + integration tests, no network, no real agent
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The tests never call GitHub, GitLab or a real agent. Forge APIs are mocked in
`src/e2e_tests.rs`, and agents are replaced by `tests/stub-agent.sh`, which makes a predictable
commit and prints the expected result block.

## Ground rules

- **Security first.** Read the [threat model](https://docs.night-porter.com/threat-model) before
  changing anything in `trust`, `agent`, `checks`, `job` or `packs`. A change that loosens a security boundary needs an issue
  first.
- **Nothing in user repos.** Night Porter must never leave files in a repository or worktree it
  works on.
- **Keep it small.** Prefer the standard library and existing dependencies. New dependencies
  need a reason in the PR.
- **TUI accessibility.** Everything must work with the keyboard alone, colour must never be the
  only signal (use text or symbols too), every view needs a CLI equivalent (`nightporter status`, `show`),
  and `NO_COLOR` must be honoured.
- One topic per PR, with a test for any new logic.
- Commits are signed (`git commit -S`).

## Packs

Want to add a review step, a reply style or an environment preset? That's usually a **pack**,
not a core change. See [Writing packs](https://docs.night-porter.com/writing-packs).

## Questions

Read the [docs](https://docs.night-porter.com), or get in touch via the [support page](https://night-porter.com/support).

## Code of Conduct

This project follows the [Contributor Covenant](https://github.com/night-porter/nightporter/blob/main/CODE_OF_CONDUCT.md).

## License

Contributions are dual-licensed under MIT OR Apache-2.0, the same as the project, unless you
say otherwise.
