//! Pre-push checks. Pure over a [`Diff`]; the caller gathers git data and optional gitleaks output.

use crate::config::{Checks, Escalation};
use crate::state::CheckResult;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use regex::RegexSet;
use std::sync::LazyLock;

/// Always protected. Includes the agent's own instruction files: an agent (or an injected
/// prompt) editing them would persist instructions into future runs.
pub const BUILTIN_PROTECTED: &[&str] = &[
    ".github/workflows/**",
    ".github/actions/**",
    ".gitlab-ci.yml",
    ".gitlab/**",
    "**/CODEOWNERS",
    "**/.env",
    "**/.env.*",
    "**/*.pem",
    "**/*.key",
    "**/id_rsa*",
    "**/id_ed25519*",
    // Other CI systems.
    ".circleci/**",
    "Jenkinsfile",
    ".travis.yml",
    "azure-pipelines.yml",
    "bitbucket-pipelines.yml",
    ".buildkite/**",
    // Agent instructions and config (any agent the user might also run).
    "CLAUDE.md",
    "**/CLAUDE.md",
    "CLAUDE.local.md",
    "**/CLAUDE.local.md",
    "AGENTS.md",
    "**/AGENTS.md",
    "GEMINI.md",
    "**/GEMINI.md",
    ".claude/**",
    ".opencode/**",
    "opencode.json",
    "opencode.jsonc",
    ".mcp.json",
    ".cursor/**",
    ".cursorrules",
    ".github/copilot-instructions.md",
    // Things that run code on checkout, commit, folder open or env start-up.
    ".husky/**",
    ".pre-commit-config.yaml",
    ".lfsconfig",
    ".envrc",
    ".vscode/**",
    ".devcontainer/**",
    ".ddev/**",
    "docker-compose*.yml",
    "docker-compose*.yaml",
    "compose*.yml",
    "compose*.yaml",
    ".git",
    ".git/**",
    "**/.git",
    "**/.git/**",
    ".gitmodules",
    "**/.gitattributes",
];

const DEP_FILES: &[&str] = &[
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "bun.lock",
    "composer.json",
    "composer.lock",
    "Cargo.toml",
    "Cargo.lock",
    "go.mod",
    "go.sum",
    "pyproject.toml",
    "poetry.lock",
    "uv.lock",
    "Pipfile",
    "Pipfile.lock",
    "Gemfile",
    "Gemfile.lock",
    "build.gradle",
    "build.gradle.kts",
    "pom.xml",
    "packages.config",
    "mix.exs",
    "mix.lock",
    "pubspec.yaml",
    "pubspec.lock",
    "Package.swift",
    "Package.resolved",
    "flake.lock",
];

static SECRETS: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"AKIA[0-9A-Z]{16}",
        r"gh[pousr]_[A-Za-z0-9]{36,}",
        r"github_pat_[A-Za-z0-9_]{50,}",
        r"glpat-[A-Za-z0-9_\-]{20,}",
        r"xox[abprs]-[A-Za-z0-9\-]{10,}",
        r"-----BEGIN (RSA |EC |DSA |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY-----",
        r"sk_live_[A-Za-z0-9]{20,}",
        r"AIza[0-9A-Za-z_\-]{35}",
        r"sk-ant-[A-Za-z0-9_\-]{20,}",
        r"sk-(proj-)?[A-Za-z0-9_\-]{32,}",
        r"(?i)(password|passwd|secret|api[_-]?key|access[_-]?token)\s*[:=]\s*['\x22][^'\x22\s$\{]{12,}['\x22]",
    ])
    .expect("secret patterns")
});

#[derive(Clone, Debug, Default)]
pub struct FileStat {
    pub path: String,
    pub added: usize,
    pub removed: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Diff {
    /// Net change, for size and display.
    pub files: Vec<FileStat>,
    /// Every path any commit in the range touched (catches add-then-revert and renames away).
    pub touched: Vec<String>,
    /// Per-commit patches with commit messages (`git log -p -U0 --text --format=%B`), for secrets.
    pub scan: String,
    /// The pre-job head is an ancestor of the new HEAD (no rewrite).
    pub fast_forward: bool,
}

impl Diff {
    pub fn lines(&self) -> usize {
        self.files.iter().map(|f| f.added + f.removed).sum()
    }
    /// All paths to check: net files plus everything touched along the way.
    fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(|f| f.path.as_str()).chain(self.touched.iter().map(String::as_str))
    }
    /// The text that can carry new content (added lines, merge resolutions, commit messages),
    /// for an external scanner.
    pub fn added_text(&self) -> String {
        self.scan_lines().collect::<Vec<_>>().join("\n")
    }
    /// Lines that can carry new content: added lines and commit messages, not removals.
    fn scan_lines(&self) -> impl Iterator<Item = &str> {
        self.scan.lines().filter(|l| !l.starts_with('-')).map(|l| l.strip_prefix('+').unwrap_or(l))
    }
}

fn globs(extra: &[String]) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for g in BUILTIN_PROTECTED.iter().copied().chain(extra.iter().map(String::as_str)) {
        // Case-insensitive: on macOS `Claude.md` *is* `CLAUDE.md`.
        if let Ok(g) = GlobBuilder::new(g).case_insensitive(true).build() {
            b.add(g);
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

/// Run all checks. `gitleaks`: `Some(findings)` if gitleaks ran. `texts`: other things that
/// will be published (replies, PR body), scanned for secrets too.
pub fn run(cfg: &Checks, d: &Diff, gitleaks: Option<&str>, texts: &[&str]) -> Vec<CheckResult> {
    let mut out = Vec::new();
    let mut push = |name: &str, ok: bool, blocking: bool, detail: String| out.push(CheckResult { name: name.into(), ok, blocking, detail });

    push(
        "history",
        d.fast_forward,
        true,
        if d.fast_forward { String::new() } else { "commits rewrite the PR history (would need a force-push)".into() },
    );

    let mut hits: Vec<String> = Vec::new();
    for l in d.scan_lines() {
        if SECRETS.is_match(l) {
            hits.push(format!("in commits: {}", redact(l)));
        }
    }
    for t in texts {
        for l in t.lines().filter(|l| SECRETS.is_match(l)) {
            hits.push(format!("in a reply: {}", redact(l)));
        }
    }
    if let Some(g) = gitleaks.filter(|g| !g.trim().is_empty()) {
        hits.push(format!("gitleaks: {}", g.lines().take(5).collect::<Vec<_>>().join("; ")));
    }
    push("secrets", hits.is_empty(), true, hits.join("\n"));

    let set = globs(&cfg.protected);
    let mut prot: Vec<&str> = d.paths().filter(|p| set.is_match(p)).collect();
    prot.dedup();
    push("protected paths", prot.is_empty(), true, prot.join(", "));

    let np: Vec<&str> = d.paths().filter(|p| p.split('/').any(|seg| seg.starts_with(".nightporter"))).collect();
    push("night porter files", np.is_empty(), true, np.join(", "));

    if cfg.escalate_on.contains(&Escalation::Deps) {
        let mut deps: Vec<&str> = d
            .paths()
            .filter(|p| {
                // Case-insensitive: on macOS `Package.json` is `package.json`.
                let name = p.rsplit('/').next().unwrap_or(p).to_ascii_lowercase();
                DEP_FILES.iter().any(|d| d.eq_ignore_ascii_case(&name))
                    || (name.starts_with("requirements") && name.ends_with(".txt"))
                    || name.ends_with(".csproj")
            })
            .collect();
        deps.dedup();
        push("dependencies", deps.is_empty(), false, deps.join(", "));
    }
    if cfg.escalate_on.contains(&Escalation::Size) {
        let n = d.lines();
        push("size", n <= cfg.max_diff_lines, false, format!("{n} lines changed (limit {})", cfg.max_diff_lines));
    }
    out
}

/// Keep enough to recognise the line, never the secret itself.
fn redact(l: &str) -> String {
    // Just enough to find the line; never enough of a token to be useful.
    let t: String = l.trim().chars().take(4).collect();
    format!("{t}… [redacted]")
}

pub fn all_ok(r: &[CheckResult]) -> bool {
    r.iter().all(|c| c.ok)
}

pub fn blocking_failed(r: &[CheckResult]) -> bool {
    r.iter().any(|c| !c.ok && c.blocking)
}

/// Parse `git diff --numstat -z --no-renames` (NUL-separated, paths unquoted).
pub fn parse_numstat_z(s: &str) -> Vec<FileStat> {
    s.split('\0')
        .filter_map(|rec| {
            let mut it = rec.trim_start_matches('\n').splitn(3, '\t');
            let (a, r, p) = (it.next()?, it.next()?, it.next()?);
            (!p.is_empty()).then(|| FileStat { path: p.to_string(), added: a.parse().unwrap_or(0), removed: r.parse().unwrap_or(0) })
        })
        .collect()
}

/// Parse `git log --name-only -z --format=` (NUL-separated paths).
pub fn parse_names_z(s: &str) -> Vec<String> {
    let mut v: Vec<String> = s.split('\0').map(|p| p.trim_matches('\n')).filter(|p| !p.is_empty()).map(String::from).collect();
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_paths_ignore_case_and_cover_env_and_agent_files() {
        for p in [
            "Claude.md",
            "docs/agents.md",
            ".DDEV/config.yaml",
            "compose.yaml",
            ".husky/pre-commit",
            "GEMINI.md",
            ".Github/Workflows/ci.yml",
        ] {
            let d = Diff { files: vec![FileStat { path: p.into(), added: 1, removed: 0 }], fast_forward: true, ..Default::default() };
            let r = run(&Checks::default(), &d, None, &[]);
            assert!(r.iter().any(|c| c.name == "protected paths" && !c.ok), "{p} should be protected");
        }
    }

    fn cfg() -> Checks {
        Checks::default()
    }
    fn diff(files: &[(&str, usize)], patch: &str) -> Diff {
        Diff {
            files: files.iter().map(|(p, n)| FileStat { path: p.to_string(), added: *n, removed: 0 }).collect(),
            touched: vec![],
            scan: patch.into(),
            fast_forward: true,
        }
    }
    fn failed(r: &[CheckResult]) -> Vec<&str> {
        r.iter().filter(|c| !c.ok).map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn clean_diff_passes() {
        let r = run(&cfg(), &diff(&[("src/main.rs", 10)], "+fn main() {}\n"), None, &[]);
        assert!(all_ok(&r), "{r:?}");
    }

    #[test]
    fn secrets_block_and_are_redacted() {
        let r = run(&cfg(), &diff(&[("a.rs", 1)], "+let t = \"ghp_abcdefghijklmnopqrstuvwxyz0123456789AB\";\n"), None, &[]);
        assert_eq!(failed(&r), vec!["secrets"]);
        assert!(blocking_failed(&r));
        assert!(!r.iter().any(|c| c.detail.contains("0123456789")));
        // Removed lines and env-var references don't count.
        let r = run(&cfg(), &diff(&[("a.rs", 1)], "-AKIAABCDEFGHIJKLMNOP\n+password = \"${DB_PASSWORD}\"\n"), None, &[]);
        assert!(all_ok(&r), "{r:?}");
    }

    #[test]
    fn protected_and_agent_files() {
        let r = run(&cfg(), &diff(&[(".github/workflows/ci.yml", 1), ("docs/CLAUDE.md", 1)], ""), None, &[]);
        assert_eq!(failed(&r), vec!["protected paths"]);
        let mut c = cfg();
        c.protected = vec!["migrations/**".into()];
        assert_eq!(failed(&run(&c, &diff(&[("migrations/001.sql", 1)], ""), None, &[])), vec!["protected paths"]);
    }

    #[test]
    fn escalations_are_non_blocking_and_configurable() {
        let r = run(&cfg(), &diff(&[("web/package.json", 1), ("big.rs", 600)], ""), None, &[]);
        assert_eq!(failed(&r), vec!["dependencies", "size"]);
        assert!(!blocking_failed(&r));
        let mut c = cfg();
        c.escalate_on.clear();
        assert!(all_ok(&run(&c, &diff(&[("package.json", 1), ("big.rs", 600)], ""), None, &[])));
    }

    #[test]
    fn history_rewrite_blocks() {
        let mut d = diff(&[("a.rs", 1)], "");
        d.fast_forward = false;
        assert!(blocking_failed(&run(&cfg(), &d, None, &[])));
    }

    #[test]
    fn np_files_block() {
        assert!(blocking_failed(&run(&cfg(), &diff(&[(".nightporter/replies.json", 1)], ""), None, &[])));
    }

    #[test]
    fn numstat_z() {
        let f = parse_numstat_z("3\t1\tsrc/a.rs\0-\t-\timg.png\0");
        assert_eq!((f[0].path.as_str(), f[0].added), ("src/a.rs", 3));
        assert_eq!(f[1].path, "img.png");
        // Non-ASCII paths stay unquoted with -z, so globs still match.
        let f = parse_numstat_z("1\t0\t.github/workflows/é.yml\0");
        assert!(blocking_failed(&run(&cfg(), &Diff { files: f, ..Default::default() }, None, &[])));
        assert_eq!(parse_names_z("\na.rs\0b.rs\0\na.rs\0"), vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn touched_paths_and_messages_count() {
        // A workflow edited then reverted is still caught; a secret in a commit message too.
        let mut d = diff(&[("src/a.rs", 1)], "");
        d.touched = vec![".github/workflows/ci.yml".into(), ".git".into()];
        assert!(blocking_failed(&run(&cfg(), &d, None, &[])));
        let d = diff(&[("src/a.rs", 1)], "fix things\n\nkey AKIAABCDEFGHIJKLMNOP\n");
        assert!(blocking_failed(&run(&cfg(), &d, None, &[])));
        let d = diff(&[("src/a.rs", 1)], "");
        assert!(blocking_failed(&run(&cfg(), &d, None, &["see ghp_abcdefghijklmnopqrstuvwxyz0123456789AB"])));
    }
}
