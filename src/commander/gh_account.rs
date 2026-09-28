/*!
[Commander] member functions that pick which `gh` account a push runs as.

Git pushes over https authenticate through `gh auth git-credential`, which
hands out the *active* `gh` account's token. With a personal and a work account
logged in, that is the wrong one for half the repos. Rather than switching the
global active account and switching back (which races with every other
terminal using `gh`, and strands you on the wrong account if jjscope dies
mid-push), the push subprocess gets `GH_TOKEN` set: `gh` prefers it over its
stored accounts, and nothing outside that one process changes.

The account is chosen from the remote's GitHub owner, in order:

1. the `jjscope.gh-accounts` config table, owner -> account (case-insensitive);
2. an account whose login equals the owner (a personal repo);
3. otherwise nothing: the push runs as whichever account is active.
*/
use std::collections::HashMap;
use std::process::Command;
use std::process::Stdio;

use crate::commander::Commander;
use crate::commander::remotes::parse_remote_list;

/// Which `gh` account a push to some remote runs as.
pub enum PushIdentity {
    /// A specific account, selected through `GH_TOKEN`.
    Account { login: String, token: String },
    /// A GitHub https remote with no matching account: the active one is used.
    Active { owner: String },
    /// Not something `gh` authenticates (ssh, another host): nothing to say.
    NotApplicable,
}

impl PushIdentity {
    /// The environment variable that makes `gh` use the selected account.
    pub fn env(&self) -> Option<(String, String)> {
        match self {
            Self::Account { token, .. } => Some(("GH_TOKEN".to_owned(), token.clone())),
            _ => None,
        }
    }

    /// A line naming the account, to show above the push's output so a wrong
    /// mapping is noticed.
    pub fn banner(&self) -> Option<String> {
        match self {
            Self::Account { login, .. } => Some(format!("Pushing as {login}")),
            Self::Active { owner } => Some(format!(
                "Pushing as the active gh account (no account matches {owner})"
            )),
            Self::NotApplicable => None,
        }
    }

    /// `output` under the banner, if any.
    pub fn with_banner(&self, output: String) -> String {
        match self.banner() {
            Some(banner) => format!("{banner}\n{output}"),
            None => output,
        }
    }
}

/// The owner of a github.com https remote URL. SSH remotes never consult `gh`,
/// so they, and other hosts, give `None`.
pub fn github_owner(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    // Drop any `user[:password]@` prefix.
    let rest = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
    let path = rest.strip_prefix("github.com/")?;
    path.split('/').next().filter(|owner| !owner.is_empty())
}

/// The login to use for `owner`: a configured mapping, else the owner itself.
pub fn candidate_login<'a>(owner: &'a str, accounts: &'a HashMap<String, String>) -> &'a str {
    accounts
        .iter()
        .find(|(configured, _)| configured.eq_ignore_ascii_case(owner))
        .map_or(owner, |(_, login)| login.as_str())
}

impl Commander {
    /// The identity a push to `remote` runs as.
    ///
    /// Every failure (no `gh`, a login `gh` does not know) is [PushIdentity::Active]
    /// or [PushIdentity::NotApplicable]: the push then behaves as it always did.
    pub fn push_identity(&self, remote: &str) -> PushIdentity {
        let Ok(list) = self.jj(["git", "remote", "list"]).run() else {
            return PushIdentity::NotApplicable;
        };
        let remotes = parse_remote_list(&list);
        let Some(owner) = remotes
            .iter()
            .find(|(name, _)| name == remote)
            .and_then(|(_, url)| github_owner(url))
        else {
            return PushIdentity::NotApplicable;
        };
        let login = candidate_login(owner, self.env.jj_config.gh_accounts());
        let token = Command::new("gh")
            .args(["auth", "token", "--hostname", "github.com", "--user", login])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|token| token.trim().to_owned())
            .filter(|token| !token.is_empty());
        match token {
            Some(token) => PushIdentity::Account {
                login: login.to_owned(),
                token,
            },
            None => PushIdentity::Active {
                owner: owner.to_owned(),
            },
        }
    }

    /// The remote `jj git push` sends to when not told: `git.push`, else
    /// `origin`. (jj sends a tracked bookmark to its own remote, which this
    /// does not look up.)
    pub fn default_push_remote(&self) -> String {
        self.jj(["config", "get", "git.push"])
            .run()
            .map(|remote| remote.trim().to_owned())
            .ok()
            .filter(|remote| !remote.is_empty())
            .unwrap_or_else(|| "origin".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_owner() {
        assert_eq!(
            github_owner("https://github.com/sswatson/jjscope.git"),
            Some("sswatson")
        );
        assert_eq!(
            github_owner("https://user@github.com/Org/repo"),
            Some("Org")
        );
        assert_eq!(github_owner("git@github.com:sswatson/jjscope.git"), None);
        assert_eq!(github_owner("https://gitlab.com/a/b.git"), None);
        assert_eq!(github_owner("https://github.com/"), None);
    }

    #[test]
    fn banner_names_the_account() {
        let account = PushIdentity::Account {
            login: "me".to_owned(),
            token: "t".to_owned(),
        };
        assert_eq!(account.with_banner("out".to_owned()), "Pushing as me\nout");
        assert!(account.env().is_some());
        let active = PushIdentity::Active {
            owner: "o".to_owned(),
        };
        assert!(
            active
                .with_banner("out".to_owned())
                .starts_with("Pushing as the active")
        );
        assert!(active.env().is_none());
        assert_eq!(
            PushIdentity::NotApplicable.with_banner("out".to_owned()),
            "out"
        );
    }

    #[test]
    fn config_beats_owner_case_insensitively() {
        let accounts = HashMap::from([("APrioriInvestments".to_owned(), "sswatson-ap".to_owned())]);
        assert_eq!(
            candidate_login("aprioriinvestments", &accounts),
            "sswatson-ap"
        );
        assert_eq!(candidate_login("sswatson", &accounts), "sswatson");
    }
}
