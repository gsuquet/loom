//! The requests of `loom pr`: the open MR of a branch, and a new draft MR. Pure: the clients
//! send them, and `loom pr -n` prints them.

use std::fmt::Write as _;

use loom_api::ForgeKind;
use serde_json::{Value, json};

/// The prefix that marks a GitLab merge request as a draft in its title.
pub const GITLAB_DRAFT_PREFIX: &str = "Draft: ";

/// A draft MR to open: the source branch of the project, into the target branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMr {
    /// The project path on the forge, such as `acme/shop` or `company-a/web/checkout`.
    pub project: String,
    /// The source branch, pushed to the project.
    pub head: String,
    /// The target branch.
    pub base: String,
    /// The title, without a draft prefix.
    pub title: String,
    /// The description, in Markdown.
    pub body: String,
}

/// A forge request: method, path and query under the REST base, and JSON body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeRequest {
    /// `GET` or `POST`.
    pub method: &'static str,
    /// The path and query, under the REST base of the host.
    pub path: String,
    /// The JSON body, `None` for a `GET`.
    pub body: Option<Value>,
}

/// The request that lists the open MRs of the branch `head` of `project`. Pure.
///
/// GitHub: `GET /repos/<project>/pulls?head=<owner>:<head>&state=open`. GitLab:
/// `GET /projects/<project, encoded>/merge_requests?source_branch=<head>&state=opened`.
#[must_use]
pub fn find_request(kind: ForgeKind, project: &str, head: &str) -> ForgeRequest {
    let path = match kind {
        ForgeKind::Github => {
            let owner = project.split('/').next().unwrap_or_default();
            format!(
                "/repos/{}/pulls?head={}&state=open&per_page=100",
                encode_path(project),
                encode(&format!("{owner}:{head}"))
            )
        }
        ForgeKind::Gitlab => format!(
            "/projects/{}/merge_requests?source_branch={}&state=opened&per_page=100",
            encode(project),
            encode(head)
        ),
    };
    ForgeRequest {
        method: "GET",
        path,
        body: None,
    }
}

/// The request that opens `mr` as a draft. Pure.
///
/// GitHub: `POST /repos/<project>/pulls` with `draft: true`. GitLab:
/// `POST /projects/<project, encoded>/merge_requests` with the `Draft: ` title prefix, which
/// GitLab reads as the draft state.
#[must_use]
pub fn create_request(kind: ForgeKind, mr: &NewMr) -> ForgeRequest {
    match kind {
        ForgeKind::Github => ForgeRequest {
            method: "POST",
            path: format!("/repos/{}/pulls", encode_path(&mr.project)),
            body: Some(json!({
                "title": mr.title,
                "head": mr.head,
                "base": mr.base,
                "body": mr.body,
                "draft": true,
            })),
        },
        ForgeKind::Gitlab => ForgeRequest {
            method: "POST",
            path: format!("/projects/{}/merge_requests", encode(&mr.project)),
            body: Some(json!({
                "source_branch": mr.head,
                "target_branch": mr.base,
                "title": gitlab_draft_title(&mr.title),
                "description": mr.body,
            })),
        },
    }
}

/// `title` with the GitLab draft prefix, once. Pure.
#[must_use]
pub fn gitlab_draft_title(title: &str) -> String {
    if title.starts_with(GITLAB_DRAFT_PREFIX) {
        title.to_owned()
    } else {
        format!("{GITLAB_DRAFT_PREFIX}{title}")
    }
}

/// `path` with each `/`-separated segment [encoded](encode). Pure.
#[must_use]
pub fn encode_path(path: &str) -> String {
    path.split('/').map(encode).collect::<Vec<_>>().join("/")
}

/// `text` percent-encoded for a path segment or a query value: every byte but
/// `[A-Za-z0-9-._~]` as `%XX`. Pure.
#[must_use]
pub fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use loom_api::ForgeKind;
    use serde_json::json;

    use super::{NewMr, create_request, encode, find_request, gitlab_draft_title};

    fn new_mr(project: &str) -> NewMr {
        NewMr {
            project: project.to_owned(),
            head: "fix/login-timeout".to_owned(),
            base: "main".to_owned(),
            title: "Login timeout on mobile checkout".to_owned(),
            body: "## Fix\n".to_owned(),
        }
    }

    #[test]
    fn the_open_mrs_of_a_branch() {
        let github = find_request(ForgeKind::Github, "acme/shop", "fix/login-timeout");
        assert_eq!(github.method, "GET");
        assert_eq!(
            github.path,
            "/repos/acme/shop/pulls?head=acme%3Afix%2Flogin-timeout&state=open&per_page=100"
        );
        assert_eq!(github.body, None);
        let gitlab = find_request(ForgeKind::Gitlab, "company-a/web/checkout", "feat/a+b");
        assert_eq!(
            gitlab.path,
            "/projects/company-a%2Fweb%2Fcheckout/merge_requests?source_branch=feat%2Fa%2Bb&state=opened&per_page=100"
        );
    }

    #[test]
    fn a_github_draft_pull_request() {
        let request = create_request(ForgeKind::Github, &new_mr("acme/shop"));
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/repos/acme/shop/pulls");
        assert_eq!(
            request.body,
            Some(json!({
                "title": "Login timeout on mobile checkout",
                "head": "fix/login-timeout",
                "base": "main",
                "body": "## Fix\n",
                "draft": true,
            }))
        );
    }

    #[test]
    fn a_gitlab_draft_merge_request() {
        let request = create_request(ForgeKind::Gitlab, &new_mr("company-a/checkout"));
        assert_eq!(
            request.path,
            "/projects/company-a%2Fcheckout/merge_requests"
        );
        assert_eq!(
            request.body,
            Some(json!({
                "source_branch": "fix/login-timeout",
                "target_branch": "main",
                "title": "Draft: Login timeout on mobile checkout",
                "description": "## Fix\n",
            }))
        );
    }

    #[test]
    fn the_draft_prefix_is_added_once() {
        assert_eq!(gitlab_draft_title("Fix"), "Draft: Fix");
        assert_eq!(gitlab_draft_title("Draft: Fix"), "Draft: Fix");
    }

    #[test]
    fn encoding() {
        assert_eq!(encode("a-Z_0.9~"), "a-Z_0.9~");
        assert_eq!(encode("a/b c:é"), "a%2Fb%20c%3A%C3%A9");
        assert_eq!(super::encode_path("acme/shop?x#y"), "acme/shop%3Fx%23y");
    }
}
