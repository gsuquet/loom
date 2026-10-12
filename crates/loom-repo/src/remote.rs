//! Remote URLs: parsing, normalization, redaction and the primary remote.

use loom_api::{Remote, RemoteUrl};
use thiserror::Error;

/// Why a remote URL has no network location.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RemoteUrlError {
    /// A local path or a `file://` URL.
    #[error("`{0}` is a local path, not a network remote")]
    Local(String),
    /// A scheme other than `ssh`, `git+ssh`, `ssh+git`, `https`, `http` or `git`.
    #[error("unsupported scheme `{scheme}` in `{url}`")]
    Scheme {
        /// The scheme, lowercase.
        scheme: String,
        /// The URL, without password.
        url: String,
    },
    /// No host.
    #[error("no host in `{0}`")]
    NoHost(String),
    /// A port that is not a number from 1 to 65535.
    #[error("invalid port in `{0}`")]
    Port(String),
    /// No repository path.
    #[error("no repository path in `{0}`")]
    NoPath(String),
}

/// Transport of a URL, which decides its default port and whether SSH aliases apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Ssh,
    Https,
    Http,
    Git,
}

impl Transport {
    fn from_scheme(scheme: &str) -> Option<Self> {
        match scheme {
            "ssh" | "git+ssh" | "ssh+git" => Some(Self::Ssh),
            "https" => Some(Self::Https),
            "http" => Some(Self::Http),
            "git" => Some(Self::Git),
            _ => None,
        }
    }

    const fn default_port(self) -> u16 {
        match self {
            Self::Ssh => 22,
            Self::Https => 443,
            Self::Http => 80,
            Self::Git => 9418,
        }
    }
}

/// Parses and normalizes a git remote URL: `[user@]host:path` (scp-like), or
/// `ssh://`, `git+ssh://`, `ssh+git://`, `https://`, `http://`, `git://` URLs with an optional
/// user and port.
///
/// The host is lowercased; for SSH transports it is first passed to `resolve_alias`, which
/// returns the real host name of an SSH alias (the caller runs `ssh -G <alias>` and caches
/// it), or `None` to keep it. The user, the default port of the transport, the `.git` suffix
/// and empty path segments are removed; the path case is kept. Pure.
///
/// # Errors
///
/// [`RemoteUrlError`] for a local path, an unsupported scheme, or a URL without host or path.
pub fn parse_remote_url(
    url: &str,
    resolve_alias: impl Fn(&str) -> Option<String>,
) -> Result<RemoteUrl, RemoteUrlError> {
    let redacted = redact_url(url);
    let url = url.trim();
    let (transport, authority, path) = if let Some((scheme, rest)) = url.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        if scheme == "file" {
            return Err(RemoteUrlError::Local(redacted));
        }
        let transport = Transport::from_scheme(&scheme).ok_or_else(|| RemoteUrlError::Scheme {
            scheme,
            url: redacted.clone(),
        })?;
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        (transport, authority, path)
    } else {
        // git's rule: scp-like when a `:` comes before any `/`, otherwise a local path.
        match url.split_once(':') {
            Some((authority, path)) if !authority.contains('/') && !authority.is_empty() => {
                (Transport::Ssh, authority, path)
            }
            _ => return Err(RemoteUrlError::Local(redacted)),
        }
    };

    // Userinfo, with any password, is dropped.
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host_port)| host_port);
    let scp_like = !url.contains("://");
    let (host, port) = split_host_port(host_port, scp_like)
        .ok_or_else(|| RemoteUrlError::Port(redacted.clone()))?;
    if host.is_empty() {
        return Err(RemoteUrlError::NoHost(redacted));
    }
    let host = match transport {
        Transport::Ssh => resolve_alias(host).unwrap_or_else(|| host.to_owned()),
        Transport::Https | Transport::Http | Transport::Git => host.to_owned(),
    }
    .to_ascii_lowercase();
    let port = port.filter(|port| *port != transport.default_port());

    let path = normalize_path(path);
    if path.is_empty() {
        return Err(RemoteUrlError::NoPath(redacted));
    }
    Ok(RemoteUrl { host, port, path })
}

/// Host and port of `[host]:port`, `host:port`, `[host]` or `host`. `None` for a port that is
/// not a number from 1 to 65535. An scp-like authority has no port.
fn split_host_port(host_port: &str, scp_like: bool) -> Option<(&str, Option<u16>)> {
    let (host, port) = if let Some(rest) = host_port.strip_prefix('[') {
        // IPv6 literal, or git's `[host]:path` form.
        let (host, after) = rest.split_once(']')?;
        (host, after.strip_prefix(':'))
    } else if scp_like {
        (host_port, None)
    } else {
        match host_port.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        }
    };
    let port = match port {
        // `host:` with nothing after: no port.
        None | Some("") => None,
        Some(port) => Some(port.parse::<u16>().ok().filter(|port| *port != 0)?),
    };
    Some((host, port))
}

/// Path without empty segments, leading or trailing `/` and `.git` suffix.
fn normalize_path(path: &str) -> String {
    let path = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    path.strip_suffix(".git").map_or_else(
        || path.clone(),
        |stripped| stripped.trim_end_matches('/').to_owned(),
    )
}

/// `url` without the password of its userinfo (`https://user:token@host/…` gives
/// `https://user@host/…`), so that remotes can be shown and stored. Other URLs are returned
/// unchanged. Pure.
#[must_use]
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return url.to_owned();
    };
    let user = userinfo.split_once(':').map_or(userinfo, |(user, _)| user);
    format!("{scheme}://{user}@{host}{path}")
}

/// A remote named `name` with the URL `url` as configured: the URL without password and its
/// location, absent when it cannot be parsed (see [`parse_remote_url`]). Pure.
#[must_use]
pub fn remote(name: &str, url: &str, resolve_alias: impl Fn(&str) -> Option<String>) -> Remote {
    Remote {
        name: name.to_owned(),
        url: redact_url(url),
        location: parse_remote_url(url, resolve_alias).ok(),
    }
}

/// The primary remote: `origin`, otherwise the first one. Pure.
#[must_use]
pub fn primary_remote(remotes: &[Remote]) -> Option<&Remote> {
    remotes
        .iter()
        .find(|remote| remote.name == "origin")
        .or_else(|| remotes.first())
}

#[cfg(test)]
mod tests {
    use super::{RemoteUrlError, parse_remote_url, primary_remote, redact_url, remote};
    use loom_api::{Remote, RemoteUrl};

    fn no_alias(_: &str) -> Option<String> {
        None
    }

    fn location(host: &str, port: Option<u16>, path: &str) -> RemoteUrl {
        RemoteUrl {
            host: host.to_owned(),
            port,
            path: path.to_owned(),
        }
    }

    #[test]
    fn parses_every_remote_format() {
        let cases: &[(&str, RemoteUrl)] = &[
            // scp-like
            (
                "git@github.com:gsuquet/loom.git",
                location("github.com", None, "gsuquet/loom"),
            ),
            (
                "github.com:gsuquet/loom",
                location("github.com", None, "gsuquet/loom"),
            ),
            (
                "git@GitHub.com:GSuquet/Loom.git",
                location("github.com", None, "GSuquet/Loom"),
            ),
            (
                "git@gitlab.com:company-a/group/sub/repo.git/",
                location("gitlab.com", None, "company-a/group/sub/repo"),
            ),
            (
                "git@[gitlab.example]:a/b.git",
                location("gitlab.example", None, "a/b"),
            ),
            // ssh://
            (
                "ssh://git@github.com/gsuquet/loom.git",
                location("github.com", None, "gsuquet/loom"),
            ),
            (
                "ssh://git@github.com:22/gsuquet/loom.git",
                location("github.com", None, "gsuquet/loom"),
            ),
            (
                "ssh://git@gitlab.example:2222/group/repo.git",
                location("gitlab.example", Some(2222), "group/repo"),
            ),
            (
                "git+ssh://git@github.com/a/b",
                location("github.com", None, "a/b"),
            ),
            (
                "ssh+git://github.com/a/b",
                location("github.com", None, "a/b"),
            ),
            (
                "SSH://git@[::1]:2222/a/b.git",
                location("::1", Some(2222), "a/b"),
            ),
            // https:// and others
            (
                "https://github.com/gsuquet/loom.git",
                location("github.com", None, "gsuquet/loom"),
            ),
            (
                "https://github.com/gsuquet/loom",
                location("github.com", None, "gsuquet/loom"),
            ),
            (
                "https://user:token@GitLab.com:443/Company-A/Repo.git",
                location("gitlab.com", None, "Company-A/Repo"),
            ),
            (
                "https://gitlab.example:8443//group//repo/",
                location("gitlab.example", Some(8443), "group/repo"),
            ),
            (
                "http://git.example:80/repo.git",
                location("git.example", None, "repo"),
            ),
            (
                "http://git.example:8080/repo",
                location("git.example", Some(8080), "repo"),
            ),
            (
                "git://git.example/repo.git",
                location("git.example", None, "repo"),
            ),
            (
                "git://git.example:9418/repo",
                location("git.example", None, "repo"),
            ),
            (
                "  https://github.com/a/b.git\n",
                location("github.com", None, "a/b"),
            ),
        ];
        for (url, expected) in cases {
            assert_eq!(
                parse_remote_url(url, no_alias).as_ref(),
                Ok(expected),
                "{url}"
            );
        }
    }

    #[test]
    fn ssh_aliases_are_resolved_through_the_injected_function() {
        let resolve = |alias: &str| match alias {
            "gh-perso" => Some("github.com".to_owned()),
            "work" => Some("GitLab.com".to_owned()),
            _ => None,
        };
        let cases: &[(&str, RemoteUrl)] = &[
            (
                "gh-perso:gsuquet/loom.git",
                location("github.com", None, "gsuquet/loom"),
            ),
            (
                "git@work:company-a/repo.git",
                location("gitlab.com", None, "company-a/repo"),
            ),
            (
                "ssh://work/company-a/repo",
                location("gitlab.com", None, "company-a/repo"),
            ),
            (
                "ssh://git@work:2222/a/b",
                location("gitlab.com", Some(2222), "a/b"),
            ),
            // Not an alias: kept.
            (
                "git@unknown.example:a/b",
                location("unknown.example", None, "a/b"),
            ),
            // HTTPS never goes through SSH aliases.
            (
                "https://work/company-a/repo",
                location("work", None, "company-a/repo"),
            ),
        ];
        for (url, expected) in cases {
            assert_eq!(
                parse_remote_url(url, resolve).as_ref(),
                Ok(expected),
                "{url}"
            );
        }
    }

    #[test]
    fn the_alias_is_given_as_written() {
        let seen = std::cell::RefCell::new(Vec::new());
        let resolve = |alias: &str| {
            seen.borrow_mut().push(alias.to_owned());
            None
        };
        parse_remote_url("git@Work-Alias:a/b", resolve).unwrap();
        parse_remote_url("https://Web/a/b", resolve).unwrap();
        assert_eq!(*seen.borrow(), ["Work-Alias"]);
    }

    #[test]
    fn rejects_urls_without_network_location() {
        let cases: &[(&str, RemoteUrlError)] = &[
            (
                "/srv/git/repo.git",
                RemoteUrlError::Local("/srv/git/repo.git".into()),
            ),
            ("../repo", RemoteUrlError::Local("../repo".into())),
            ("./a:b", RemoteUrlError::Local("./a:b".into())),
            ("repo", RemoteUrlError::Local("repo".into())),
            (":repo", RemoteUrlError::Local(":repo".into())),
            (
                "file:///srv/repo.git",
                RemoteUrlError::Local("file:///srv/repo.git".into()),
            ),
            (
                "ftp://host/repo",
                RemoteUrlError::Scheme {
                    scheme: "ftp".into(),
                    url: "ftp://host/repo".into(),
                },
            ),
            (
                "https:///a/b",
                RemoteUrlError::NoHost("https:///a/b".into()),
            ),
            ("git@:a/b", RemoteUrlError::NoHost("git@:a/b".into())),
            (
                "https://host:http/a",
                RemoteUrlError::Port("https://host:http/a".into()),
            ),
            (
                "https://host:0/a",
                RemoteUrlError::Port("https://host:0/a".into()),
            ),
            (
                "https://host:65536/a",
                RemoteUrlError::Port("https://host:65536/a".into()),
            ),
            (
                "https://github.com",
                RemoteUrlError::NoPath("https://github.com".into()),
            ),
            (
                "https://github.com/.git",
                RemoteUrlError::NoPath("https://github.com/.git".into()),
            ),
            (
                "git@github.com:",
                RemoteUrlError::NoPath("git@github.com:".into()),
            ),
        ];
        for (url, expected) in cases {
            assert_eq!(
                parse_remote_url(url, no_alias).as_ref(),
                Err(expected),
                "{url}"
            );
        }
    }

    #[test]
    fn errors_never_show_a_password() {
        let error = parse_remote_url("https://me:s3cr3t@host", no_alias).unwrap_err();
        assert_eq!(error, RemoteUrlError::NoPath("https://me@host".into()));
        assert!(!error.to_string().contains("s3cr3t"));
    }

    #[test]
    fn redacts_passwords_only() {
        for (url, redacted) in [
            (
                "https://me:s3cr3t@github.com/a/b",
                "https://me@github.com/a/b",
            ),
            (
                "https://me:p@ss@github.com/a/b",
                "https://me@github.com/a/b",
            ),
            ("https://me@github.com/a/b", "https://me@github.com/a/b"),
            ("https://github.com/a/b", "https://github.com/a/b"),
            ("https://github.com/a@b", "https://github.com/a@b"),
            ("git@github.com:a/b.git", "git@github.com:a/b.git"),
            ("/srv/repo", "/srv/repo"),
        ] {
            assert_eq!(redact_url(url), redacted, "{url}");
        }
    }

    #[test]
    fn remote_keeps_the_redacted_url_and_its_location() {
        let parsed = remote("origin", "https://me:tok@github.com/a/b.git", no_alias);
        assert_eq!(
            parsed,
            Remote {
                name: "origin".to_owned(),
                url: "https://me@github.com/a/b.git".to_owned(),
                location: Some(location("github.com", None, "a/b")),
            }
        );
        let local = remote("backup", "/srv/backup.git", no_alias);
        assert_eq!(local.location, None);
        assert_eq!(local.url, "/srv/backup.git");
    }

    #[test]
    fn primary_remote_is_origin_otherwise_the_first() {
        let named = |names: &[&str]| -> Vec<Remote> {
            names
                .iter()
                .map(|name| remote(name, "git@github.com:a/b", no_alias))
                .collect()
        };
        let name = |remotes: &[Remote]| primary_remote(remotes).map(|remote| remote.name.clone());
        assert_eq!(
            name(&named(&["upstream", "origin"])),
            Some("origin".to_owned())
        );
        assert_eq!(
            name(&named(&["upstream", "fork"])),
            Some("upstream".to_owned())
        );
        assert_eq!(name(&named(&[])), None);
    }
}
