//! Who may ask Night Porter for work. Pure: team membership is looked up by the caller.

use crate::config::Trust;
use crate::forge::Role;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Trusted(&'static str),
    /// Unknown author: goes to the inbox for the user to decide.
    Untrusted,
    /// Explicitly denied: dropped.
    Denied,
}

impl Decision {
    #[cfg(test)]
    pub fn is_trusted(&self) -> bool {
        matches!(self, Decision::Trusted(_))
    }
}

/// Order: deny lists → allow lists → role → open → untrusted.
pub fn decide(t: &Trust, author: &str, role: Role, is_bot: bool, in_team: impl Fn(&str) -> bool) -> Decision {
    let eq = |list: &[String]| list.iter().any(|u| u.eq_ignore_ascii_case(author));
    if eq(&t.deny_users) || t.deny_teams.iter().any(|team| in_team(team)) {
        return Decision::Denied;
    }
    if is_bot {
        // Bots are never trusted by role (a bot can be installed by anyone with access).
        return if eq(&t.allow_bots) { Decision::Trusted("allow_bots") } else { Decision::Denied };
    }
    if eq(&t.allow_users) {
        return Decision::Trusted("allow_users");
    }
    if t.allow_teams.iter().any(|team| in_team(team)) {
        return Decision::Trusted("allow_teams");
    }
    if t.roles.contains(&role) {
        return Decision::Trusted("role");
    }
    if t.open {
        return Decision::Trusted("open");
    }
    Decision::Untrusted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trust(extra: &str) -> Trust {
        toml::from_str(extra).unwrap()
    }
    const NO_TEAM: fn(&str) -> bool = |_| false;

    #[test]
    fn roles_trusted_by_default() {
        let t = trust("");
        assert_eq!(decide(&t, "alice", Role::Member, false, NO_TEAM), Decision::Trusted("role"));
        assert_eq!(decide(&t, "rando", Role::None, false, NO_TEAM), Decision::Untrusted);
        assert_eq!(decide(&t, "contrib", Role::Contributor, false, NO_TEAM), Decision::Untrusted);
    }

    #[test]
    fn deny_beats_everything() {
        let t = trust(
            r#"deny_users = ["Mallory"]
allow_users = ["mallory"]
deny_teams = ["org/bad"]"#,
        );
        assert_eq!(decide(&t, "mallory", Role::Owner, false, NO_TEAM), Decision::Denied);
        assert_eq!(decide(&t, "bob", Role::Owner, false, |team| team == "org/bad"), Decision::Denied);
    }

    #[test]
    fn allow_lists() {
        let t = trust(
            r#"allow_users = ["Carol"]
allow_teams = ["org/platform"]
roles = []"#,
        );
        assert!(decide(&t, "carol", Role::None, false, NO_TEAM).is_trusted());
        assert!(decide(&t, "dave", Role::None, false, |team| team == "org/platform").is_trusted());
        assert_eq!(decide(&t, "erin", Role::Owner, false, NO_TEAM), Decision::Untrusted);
    }

    #[test]
    fn bots_need_explicit_allow() {
        let t = trust(r#"allow_bots = ["coderabbitai[bot]"]"#);
        assert!(decide(&t, "coderabbitai[bot]", Role::None, true, NO_TEAM).is_trusted());
        assert_eq!(decide(&t, "dependabot[bot]", Role::Member, true, NO_TEAM), Decision::Denied);
    }

    #[test]
    fn open_mode() {
        let t = trust("open = true");
        assert_eq!(decide(&t, "rando", Role::None, false, NO_TEAM), Decision::Trusted("open"));
    }
}
