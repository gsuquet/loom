use loom_core::{
    Actor, ContextName, EventLog, ExternalRef, Kind, Link, MachineId, NewTask, Origin, Relations,
    Status, TaskDocument, Timestamp, create, render_task_md,
};

use super::{IndexedState, IndexedTask, link_keys};
use crate::port::{StoredTask, TaskFile};

fn context(name: &str) -> ContextName {
    name.parse().unwrap()
}

fn stored(text: &str, task: &loom_core::Task, events: EventLog) -> StoredTask {
    StoredTask {
        id: task.id.clone(),
        context: task.context.clone(),
        file: TaskFile::parse(text.as_bytes().to_vec(), &task.id, &task.context),
        events,
    }
}

fn task(kind: Kind, links: Vec<Link>) -> (loom_core::Task, EventLog) {
    let mut bits = 7_u64;
    let mut random = move || {
        bits = bits.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        bits
    };
    let (task, events) = create(
        NewTask {
            title: "Écrire le talk".to_owned(),
            kind,
            context: context("work"),
            status: Status::Ready,
            importance: None,
            due: None,
            pinned: false,
            links,
            relations: Relations::default(),
        },
        &mut random,
        &[],
        Timestamp::from_unix_secs(100).unwrap(),
        &Origin {
            actor: Actor::User,
            machine_id: MachineId("laptop".to_owned()),
        },
    )
    .unwrap();
    (
        task,
        EventLog {
            events,
            skipped: Vec::new(),
        },
    )
}

fn external(provider: &str, key: &str) -> ExternalRef {
    ExternalRef {
        provider: provider.to_owned(),
        key: key.to_owned(),
        url: Some(format!("https://{provider}.example/{key}")),
    }
}

#[test]
fn link_keys_are_the_keys_of_origin_and_related_links() {
    let links = [
        Link::Origin(external("jira", "CORE-1042")),
        Link::Repo {
            repo: "work/checkout".to_owned(),
        },
        Link::Related(external("gitlab", "!88")),
    ];
    assert_eq!(link_keys(&links), "CORE-1042 !88");
    assert_eq!(link_keys(&[]), "");
}

#[test]
fn a_valid_file_indexes_its_task_and_notes() {
    let (task, events) = task(
        Kind::Content { format: None },
        vec![Link::Origin(external("jira", "CORE-7"))],
    );
    let text = render_task_md(&TaskDocument::new(&task));
    let indexed = IndexedTask::from_stored(&stored(&text, &task, events));
    assert_eq!(indexed.id, task.id);
    assert_eq!(indexed.context, context("work"));
    assert_eq!(indexed.hash, crate::port::ContentHash::of(text.as_bytes()));
    assert_eq!(
        indexed.state,
        IndexedState::Valid {
            task: Box::new(task),
            body: "## Audience\n\n## Outline\n\n## Delivery\n".to_owned(),
        }
    );
    assert_eq!(indexed.title(), "Écrire le talk");
    assert_eq!(indexed.link_keys(), "CORE-7");
    assert!(!indexed.read_only());
}

#[test]
fn an_invalid_file_is_flagged_read_only_with_its_text() {
    let (task, events) = task(Kind::Chore, Vec::new());
    let text = "+++\ntitle = \n+++\nmy notes\n";
    let indexed = IndexedTask::from_stored(&stored(text, &task, events));
    let IndexedState::Invalid { error, text: kept } = &indexed.state else {
        panic!("not invalid: {indexed:?}");
    };
    assert!(error.starts_with("line 2: invalid TOML"), "{error}");
    assert_eq!(kept, text);
    assert_eq!(indexed.title(), "");
    assert_eq!(indexed.link_keys(), "");
    assert!(indexed.read_only());
}

#[test]
fn a_task_of_an_unknown_kind_is_valid_but_read_only() {
    let (task, events) = task(Kind::Chore, Vec::new());
    let text = render_task_md(&TaskDocument::new(&task)).replace("\"chore\"", "\"meeting\"");
    let indexed = IndexedTask::from_stored(&stored(&text, &task, events));
    assert!(matches!(indexed.state, IndexedState::Valid { .. }));
    assert!(indexed.read_only());
}
