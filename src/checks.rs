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
    ".forgejo/**",
    ".gitea/**",
    ".woodpecker/**",
    ".woodpecker.yml",
    ".woodpecker.yaml",
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
    ".codex/**",
    ".gemini/**",
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

// ASCII only (`\d` is Unicode in `regex`): the checksums below slice bytes.
static CARD: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\b[0-9]+(?:[ -][0-9]+)*\b").expect("card"));
static IBAN: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\b[A-Z]{2}[0-9]{2}[A-Z0-9]*(?: [A-Z0-9]+)*\b").expect("iban"));
/// Card numbers and IBANs payment SDKs and docs use as examples.
const EXAMPLE_PII: &[&str] = &[
    "4242424242424242",
    "4111111111111111",
    "4000056655665556",
    "4000000000000002",
    "4000000000009995",
    "4000000000000077",
    "5555555555554444",
    "5105105105105100",
    "5200828282828210",
    "378282246310005",
    "371449635398431",
    "6011111111111117",
    "3566002020360505",
    "GB82WEST12345698765432",
    "DE89370400440532013000",
];

fn luhn(d: &str) -> bool {
    let sum: u32 = d
        .bytes()
        .rev()
        .enumerate()
        .map(|(i, b)| {
            let n = u32::from(b - b'0');
            if i % 2 == 1 { if n > 4 { n * 2 - 9 } else { n * 2 } } else { n }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// A real-looking card number: 13-19 digits, a card network's first digit, not one digit repeated.
fn card_ok(d: &str) -> bool {
    (13..=19).contains(&d.len()) && matches!(d.as_bytes()[0], b'2'..=b'6') && d.bytes().any(|b| b != d.as_bytes()[0]) && luhn(d)
}

fn iban_ok(s: &str) -> bool {
    if !(15..=34).contains(&s.len()) {
        return false;
    }
    let (head, rest) = s.split_at(4);
    rest.chars().chain(head.chars()).try_fold(0u32, |acc, c| {
        let v = c.to_digit(36)?;
        Some(if v < 10 { (acc * 10 + v) % 97 } else { (acc * 100 + v) % 97 })
    }) == Some(1)
}

/// Every run of consecutive space/dash-separated groups in `m`, joined, that `ok` accepts. A
/// number is written as one run or as groups, so this finds it next to an expiry date, a CVV or
/// a following word without matching inside a longer number.
/// ponytail: O(groups²) per match; matches are short.
fn group_runs(m: &str, from_start: bool, ok: impl Fn(&str) -> bool) -> Option<String> {
    let g: Vec<&str> = m.split([' ', '-']).collect();
    let starts = if from_start { 0..1 } else { 0..g.len() };
    starts.flat_map(|a| (a + 1..=g.len()).map(move |b| (a, b))).map(|(a, b)| g[a..b].concat()).find(|c| ok(c))
}

/// Card numbers (Luhn-valid) and IBANs (mod-97-valid) in `text`, redacted.
/// ponytail: checksums only, two kinds; names, emails and phones would hold every other reply.
pub fn personal_data(text: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for m in CARD.find_iter(text) {
        if let Some(d) = group_runs(m.as_str(), false, |c| card_ok(c) && !EXAMPLE_PII.contains(&c)) {
            hits.push(format!("card number {}… [redacted]", &d[..4]));
        }
    }
    for m in IBAN.find_iter(text) {
        if let Some(s) = group_runs(m.as_str(), true, |c| iban_ok(c) && !EXAMPLE_PII.contains(&c)) {
            hits.push(format!("IBAN {}… [redacted]", &s[..4]));
        }
    }
    hits
}

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

/// Env-file variants (`.env.example`, `.env.local`, …). Unprotected by `checks.allow_env_variants`.
const ENV_VARIANTS: &str = "**/.env.*";

/// The protected globs for a repo: built-ins plus its extras. `allow_env_variants` drops only
/// [`ENV_VARIANTS`]: a bare `.env` stays protected and the secrets scan still reads every line.
pub fn protected_globs(c: &Checks) -> impl Iterator<Item = &str> {
    let skip_env = c.allow_env_variants;
    BUILTIN_PROTECTED.iter().copied().filter(move |g| !(skip_env && *g == ENV_VARIANTS)).chain(c.protected.iter().map(String::as_str))
}

fn globs(c: &Checks) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for g in protected_globs(c) {
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
    hits.extend(reply_secrets(texts));
    if let Some(g) = gitleaks.filter(|g| !g.trim().is_empty()) {
        hits.push(format!("gitleaks: {}", g.lines().take(5).collect::<Vec<_>>().join("; ")));
    }
    push("secrets", hits.is_empty(), true, hits.join("\n"));

    // Replies go out as you: customer data copied from a log or fixture waits for your OK.
    let pii: Vec<String> = texts.iter().flat_map(|t| personal_data(t)).collect();
    push(PII, pii.is_empty(), false, pii.join("\n"));

    let set = globs(cfg);
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
                    || (name.starts_with("requirements") && ends_with_ci(&name, ".txt"))
                    || ends_with_ci(&name, ".csproj")
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

const PII: &str = "personal data";
const REPLY_HIT: &str = "in a reply: ";

fn reply_secrets(texts: &[&str]) -> Vec<String> {
    texts.iter().flat_map(|t| t.lines()).filter(|l| SECRETS.is_match(l)).map(|l| format!("{REPLY_HIT}{}", redact(l))).collect()
}

/// Redo the reply parts of the checks after you edited the replies: the secrets found in replies
/// and the personal-data check. Findings in commits stay as they were.
pub fn recheck_replies(results: &mut Vec<CheckResult>, texts: &[&str]) {
    let pii: Vec<String> = texts.iter().flat_map(|t| personal_data(t)).collect();
    let fresh = [("secrets", reply_secrets(texts), true), (PII, pii, false)];
    for (name, hits, blocking) in fresh {
        let i = results.iter().position(|c| c.name == name).unwrap_or_else(|| {
            results.push(CheckResult { name: name.into(), ok: true, blocking, detail: String::new() });
            results.len() - 1
        });
        let c = &mut results[i];
        let mut lines: Vec<String> =
            if name == PII { vec![] } else { c.detail.lines().filter(|l| !l.starts_with(REPLY_HIT)).map(String::from).collect() };
        lines.extend(hits);
        c.ok = lines.is_empty();
        c.detail = lines.join("\n");
    }
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

/// `s.ends_with(suffix)`, ignoring ASCII case (macOS filesystems are case-insensitive).
pub(crate) fn ends_with_ci(s: &str, suffix: &str) -> bool {
    s.len() >= suffix.len() && s.as_bytes()[s.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_match_ignores_case() {
        assert!(ends_with_ci("x/Run.SH", ".sh") && ends_with_ci(".sh", ".sh") && ends_with_ci("a.Csproj", ".csproj"));
        assert!(!ends_with_ci("sh", ".sh") && !ends_with_ci("é", ".sh") && !ends_with_ci("a.shx", ".sh"));
    }

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

    #[test]
    fn allow_env_variants_unprotects_env_star_only() {
        let hit = |c: &Checks, p: &str| {
            let d = Diff { files: vec![FileStat { path: p.into(), added: 1, removed: 0 }], fast_forward: true, ..Default::default() };
            run(c, &d, None, &[]).iter().any(|r| r.name == "protected paths" && !r.ok)
        };
        let on = Checks { allow_env_variants: true, ..Checks::default() };
        assert!(hit(&Checks::default(), ".env.example"));
        assert!(!hit(&on, ".env.example") && !hit(&on, "app/.env.dist"));
        assert!(hit(&on, ".env") && hit(&on, "sub/.env") && hit(&on, ".envrc"));
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
    fn personal_data_in_replies_needs_approval() {
        let d = diff(&[("a.rs", 1)], "+// 4539 1488 0343 6467\n");
        let r = run(&cfg(), &d, None, &["Refund card 4539-1488-0343-6467 12/27 and IBAN NL91 ABNA 0417 1643 00 BIC ABNANL2A."]);
        assert_eq!(failed(&r), vec!["personal data"]);
        assert!(!blocking_failed(&r));
        let detail = &r.iter().find(|c| c.name == "personal data").unwrap().detail;
        assert!(detail.contains("card") && detail.contains("IBAN") && !detail.contains("6467"), "{detail}");
        assert_eq!(personal_data("paid with 12 4539 1488 0343 6467 today").len(), 1, "a group before it");
        // The diff isn't scanned; example numbers, bad checksums, zeros, ids and non-ASCII digits don't count.
        let ok = "Tested with 4242 4242 4242 4242, 4000 0000 0000 0002, GB82 WEST 1234 5698 7654 32; order \
                  1234567890123, 0000000000000000, 1700000000000 and ٤٥٣٩١٤٨٨٠٣٤٣٦٤٦٧ or DE٨٩370400440532013000.";
        assert!(all_ok(&run(&cfg(), &d, None, &[ok])), "{ok}");
    }

    #[test]
    fn recheck_replaces_only_the_reply_findings() {
        let d = diff(&[("a.rs", 1)], "+AKIAABCDEFGHIJKLMNOP\n");
        let mut r = run(&cfg(), &d, None, &["key AKIAQRSTUVWXYZ012345, card 4539 1488 0343 6467"]);
        assert_eq!(failed(&r), vec!["secrets", "personal data"]);
        recheck_replies(&mut r, &["all fixed"]);
        assert_eq!(failed(&r), vec!["secrets"], "the commit's secret stays");
        assert!(!r[1].detail.contains("in a reply"), "{}", r[1].detail);
        let mut clean = run(&cfg(), &diff(&[("a.rs", 1)], "+ok\n"), None, &[]);
        recheck_replies(&mut clean, &["sk-ant-abcdefghijklmnopqrstuvwxyz"]);
        assert!(blocking_failed(&clean));
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
