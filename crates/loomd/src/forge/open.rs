//! `POST /v1/tasks/{id}/mr` (`loom pr`): the draft MR of a branch, opened or found, then linked
//! to the task. The decisions are pure; [`open_or_link`] calls the forge.

use loom_api::{ContextName, ExternalRef, ForgeRequestPlan, Link, MrAction, MrOpenRequest, Task};
use loom_core::body_template;
use loom_forge::client::{ForgeError, OpenForge};
use loom_forge::model::MrSummary;
use loom_forge::open::{ForgeRequest, NewMr, create_request, find_request};

use super::check::ForgeTarget;

/// Why a request of `loom pr` is refused before any forge call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenRefusal {
    /// A field of the request is empty.
    #[error("`{0}` is empty")]
    Empty(&'static str),
    /// The project path is not a path of the forge.
    #[error("`{0}` is not a project path of the forge")]
    InvalidProject(String),
    /// The source and the target branch are the same.
    #[error("the branch `{0}` is the target branch: create a branch for the change first")]
    SameBranch(String),
    /// The context of the task has no credential for the host.
    #[error(
        "context `{context}` has no forge credential for `{host}`: add token_cmd or token_env to [forges.\"{host}\"] or [contexts.{context}.forges.\"{host}\"]"
    )]
    NoCredential {
        /// The context of the task.
        context: ContextName,
        /// The host of the remote.
        host: String,
    },
}

/// The credential of `context` for `host`, from `targets`, after the checks of `request`. Pure.
///
/// # Errors
///
/// [`OpenRefusal`].
pub fn target_of<'a>(
    targets: &'a [ForgeTarget],
    context: &ContextName,
    request: &MrOpenRequest,
) -> Result<&'a ForgeTarget, OpenRefusal> {
    for (name, value) in [
        ("host", &request.host),
        ("project", &request.project),
        ("head", &request.head),
        ("base", &request.base),
    ] {
        if value.trim().is_empty() {
            return Err(OpenRefusal::Empty(name));
        }
    }
    let segments: Vec<&str> = request.project.split('/').collect();
    if segments.len() < 2
        || segments
            .iter()
            .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
    {
        return Err(OpenRefusal::InvalidProject(request.project.clone()));
    }
    if request.head == request.base {
        return Err(OpenRefusal::SameBranch(request.head.clone()));
    }
    targets
        .iter()
        .find(|target| target.context == *context && target.host == request.host)
        .ok_or_else(|| OpenRefusal::NoCredential {
            context: context.clone(),
            host: request.host.clone(),
        })
}

/// The MR to open for `task`: its title, and a description made of the template of its kind
/// and its origin and related links. The notes of the task are private and never sent. Pure.
#[must_use]
pub fn new_mr(task: &Task, request: &MrOpenRequest) -> NewMr {
    NewMr {
        project: request.project.clone(),
        head: request.head.clone(),
        base: request.base.clone(),
        title: task.title.clone(),
        body: mr_body(task),
    }
}

fn mr_body(task: &Task) -> String {
    let mut body = body_template(&task.kind).to_owned();
    let links: Vec<String> = task
        .links
        .iter()
        .filter_map(|link| match link {
            Link::Origin(external) | Link::Related(external) => Some(link_line(external)),
            Link::Delivery(_) | Link::Repo { .. } => None,
        })
        .collect();
    if !links.is_empty() {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str("## Links\n\n");
        for line in links {
            body.push_str(&line);
            body.push('\n');
        }
    }
    body
}

fn link_line(external: &ExternalRef) -> String {
    external.url.as_ref().map_or_else(
        || format!("- {} {}", external.provider, external.key),
        |url| format!("- {} [{}]({url})", external.provider, external.key),
    )
}

/// `request` with its full URL under the API of `target`. Pure.
#[must_use]
pub fn plan(target: &ForgeTarget, request: &ForgeRequest) -> ForgeRequestPlan {
    ForgeRequestPlan {
        method: request.method.to_owned(),
        url: format!("{}{}", target.api.rest, request.path),
        body: request.body.clone(),
    }
}

/// The requests of `mr`: the search of the open MRs of its branch, then its creation. Pure.
#[must_use]
pub fn requests(target: &ForgeTarget, mr: &NewMr) -> [ForgeRequestPlan; 2] {
    [
        plan(target, &find_request(target.kind, &mr.project, &mr.head)),
        plan(target, &create_request(target.kind, mr)),
    ]
}

/// The `delivery` link of `mr`, opened on a forge of `target`. Pure.
#[must_use]
pub fn delivery(target: &ForgeTarget, mr: &MrSummary) -> ExternalRef {
    ExternalRef {
        provider: target.kind.as_str().to_owned(),
        key: mr.key.to_string(),
        url: Some(mr.web_url.clone()),
    }
}

/// What the forge did: the MR, and the requests sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// Opened, or an open MR of the branch linked.
    pub action: MrAction,
    /// The MR.
    pub mr: MrSummary,
    /// The requests sent, in order.
    pub sent: Vec<ForgeRequestPlan>,
}

/// Finds the open MR of the branch of `mr`, or opens it as a draft. The first open MR of the
/// branch wins; none is opened then.
///
/// # Errors
///
/// [`ForgeError`] of the forge.
pub fn open_or_link(
    target: &ForgeTarget,
    forge: &dyn OpenForge,
    mr: &NewMr,
) -> Result<Opened, ForgeError> {
    let [find, create] = requests(target, mr);
    let found = forge.open_mrs_of_branch(&mr.project, &mr.head)?;
    if let Some(existing) = found.into_iter().next() {
        return Ok(Opened {
            action: MrAction::Linked,
            mr: existing,
            sent: vec![find],
        });
    }
    let created = forge.create_draft_mr(mr)?;
    Ok(Opened {
        action: MrAction::Opened,
        mr: created,
        sent: vec![find, create],
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use loom_api::{
        ContextName, CredentialOrigin, ExternalRef, ForgeKind, Importance, Kind, Link, MrAction,
        MrOpenRequest, Relations, Status, Task, Timestamp,
    };
    use loom_config::forges::TokenSource;
    use loom_forge::client::{ApiBase, Forge, ForgeError, OpenForge, TokenUser};
    use loom_forge::model::{MrKey, MrState, MrSummary};
    use loom_forge::open::NewMr;

    use super::{ForgeTarget, OpenRefusal, new_mr, open_or_link, target_of};

    fn context(name: &str) -> ContextName {
        name.parse().unwrap()
    }

    fn target(kind: ForgeKind, host: &str) -> ForgeTarget {
        ForgeTarget {
            context: context("company-a"),
            host: host.to_owned(),
            kind,
            api: ApiBase {
                rest: "https://api.example".to_owned(),
                graphql: None,
            },
            origin: CredentialOrigin::Host,
            source: TokenSource::Env("TOKEN".to_owned()),
        }
    }

    fn request() -> MrOpenRequest {
        MrOpenRequest {
            host: "gitlab.com".to_owned(),
            project: "company-a/checkout".to_owned(),
            head: "fix/login-timeout".to_owned(),
            base: "main".to_owned(),
            dry_run: false,
        }
    }

    fn task(links: Vec<Link>) -> Task {
        let at = Timestamp::from_unix_secs(1_760_000_000).unwrap();
        Task {
            id: "k3q7zm2a".parse().unwrap(),
            title: "Login timeout on mobile checkout".to_owned(),
            kind: Kind::Bug,
            context: context("company-a"),
            status: Status::Active,
            importance: Importance::P1,
            due: None,
            pinned: false,
            links,
            relations: Relations::default(),
            created: at,
            updated: at,
        }
    }

    #[test]
    fn the_credential_is_the_one_of_the_context_of_the_task_and_the_host() {
        let targets = [
            target(ForgeKind::Github, "github.com"),
            target(ForgeKind::Gitlab, "gitlab.com"),
        ];
        let found = target_of(&targets, &context("company-a"), &request()).unwrap();
        assert_eq!(found.host, "gitlab.com");
        assert_eq!(
            target_of(&targets, &context("personal"), &request()).unwrap_err(),
            OpenRefusal::NoCredential {
                context: context("personal"),
                host: "gitlab.com".to_owned(),
            }
        );
        let same = MrOpenRequest {
            base: "fix/login-timeout".to_owned(),
            ..request()
        };
        assert!(matches!(
            target_of(&targets, &context("company-a"), &same),
            Err(OpenRefusal::SameBranch(_))
        ));
        for project in ["shop", "acme/../shop", "acme//shop"] {
            let invalid = MrOpenRequest {
                project: project.to_owned(),
                ..request()
            };
            assert_eq!(
                target_of(&targets, &context("company-a"), &invalid).unwrap_err(),
                OpenRefusal::InvalidProject(project.to_owned())
            );
        }
        let empty = MrOpenRequest {
            project: " ".to_owned(),
            ..request()
        };
        assert_eq!(
            target_of(&targets, &context("company-a"), &empty).unwrap_err(),
            OpenRefusal::Empty("project")
        );
    }

    #[test]
    fn the_description_is_the_kind_template_and_the_links_never_the_notes() {
        let links = vec![
            Link::Origin(ExternalRef {
                provider: "jira".to_owned(),
                key: "CORE-1042".to_owned(),
                url: Some("https://jira.example/browse/CORE-1042".to_owned()),
            }),
            Link::Related(ExternalRef {
                provider: "datadog".to_owned(),
                key: "monitor-7".to_owned(),
                url: None,
            }),
            Link::Repo {
                repo: "company-a/checkout".to_owned(),
            },
        ];
        let mr = new_mr(&task(links), &request());
        assert_eq!(mr.title, "Login timeout on mobile checkout");
        assert_eq!(
            mr.body,
            "## Symptoms\n\n## Cause\n\n## Fix\n\n## Verification\n\n## Links\n\n- jira [CORE-1042](https://jira.example/browse/CORE-1042)\n- datadog monitor-7\n"
        );
        assert_eq!(
            new_mr(&task(Vec::new()), &request()).body,
            "## Symptoms\n\n## Cause\n\n## Fix\n\n## Verification\n"
        );
    }

    /// A forge with `open` MRs on the branch; records the calls.
    struct Fake {
        open: Vec<MrSummary>,
        calls: RefCell<Vec<&'static str>>,
    }

    fn summary(number: u64) -> MrSummary {
        MrSummary {
            key: MrKey::new(ForgeKind::Gitlab, "company-a/checkout", number).unwrap(),
            title: "Login timeout on mobile checkout".to_owned(),
            author: "me".to_owned(),
            state: MrState::Open,
            draft: true,
            head_sha: None,
            web_url: format!("https://gitlab.com/company-a/checkout/-/merge_requests/{number}"),
            updated_at: Timestamp::from_unix_secs(1_760_000_000).unwrap(),
        }
    }

    impl Forge for Fake {
        fn user(&self) -> Result<TokenUser, ForgeError> {
            Err(ForgeError::Unexpected("not called".to_owned()))
        }
    }

    impl OpenForge for Fake {
        fn open_mrs_of_branch(
            &self,
            _project: &str,
            _head: &str,
        ) -> Result<Vec<MrSummary>, ForgeError> {
            self.calls.borrow_mut().push("find");
            Ok(self.open.clone())
        }

        fn create_draft_mr(&self, _mr: &NewMr) -> Result<MrSummary, ForgeError> {
            self.calls.borrow_mut().push("create");
            Ok(summary(91))
        }
    }

    #[test]
    fn an_open_mr_of_the_branch_is_linked_instead_of_a_new_one() {
        let target = target(ForgeKind::Gitlab, "gitlab.com");
        let mr = new_mr(&task(Vec::new()), &request());
        let fake = Fake {
            open: vec![summary(88)],
            calls: RefCell::default(),
        };
        let linked = open_or_link(&target, &fake, &mr).unwrap();
        assert_eq!(linked.action, MrAction::Linked);
        assert_eq!(linked.mr.key.number(), 88);
        assert_eq!(linked.sent.len(), 1);
        assert_eq!(*fake.calls.borrow(), ["find"]);

        let fake = Fake {
            open: Vec::new(),
            calls: RefCell::default(),
        };
        let opened = open_or_link(&target, &fake, &mr).unwrap();
        assert_eq!(opened.action, MrAction::Opened);
        assert_eq!(opened.mr.key.number(), 91);
        assert_eq!(
            opened
                .sent
                .iter()
                .map(|sent| sent.method.as_str())
                .collect::<Vec<_>>(),
            ["GET", "POST"]
        );
        assert_eq!(
            opened.sent[1].url,
            "https://api.example/projects/company-a%2Fcheckout/merge_requests"
        );
        assert_eq!(*fake.calls.borrow(), ["find", "create"]);
    }
}
