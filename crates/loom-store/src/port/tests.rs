use loom_core::{
    Actor, ContextName, Event, EventId, EventRecord, Kind, MachineId, NewTask, Origin, Status,
    TaskDocument, TaskId, Timestamp, create, parse_events, render_event, render_task_md,
};

use super::{
    ContentHash, InvalidTask, StoredTask, TaskFile, TaskLocation, appended, archive_year_dir,
    task_dir_id,
};

fn at(secs: u64) -> Timestamp {
    Timestamp::from_unix_secs(secs).unwrap()
}

fn context(name: &str) -> ContextName {
    name.parse().unwrap()
}

fn origin() -> Origin {
    Origin {
        actor: Actor::User,
        machine_id: MachineId("laptop".to_owned()),
    }
}

/// A new task of `work` created at `at(100)`, and its `created` event.
fn new_task() -> (loom_core::Task, Vec<EventRecord>) {
    let mut bits = 0x1234_5678_9abc_def0_u64;
    let mut random = move || {
        bits = bits.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        bits
    };
    create(
        NewTask {
            title: "Fix the login".to_owned(),
            kind: Kind::Bug,
            context: context("work"),
            status: Status::Inbox,
            importance: None,
            due: None,
            pinned: false,
            links: Vec::new(),
            relations: loom_core::Relations::default(),
        },
        &mut random,
        &[],
        at(100),
        &origin(),
    )
    .unwrap()
}

#[test]
fn a_content_hash_is_the_sha256_of_the_bytes_in_hex() {
    assert_eq!(
        ContentHash::of(b"").as_str(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(ContentHash::of(b"a"), ContentHash::of(b"a"));
    assert_ne!(ContentHash::of(b"a"), ContentHash::of(b"b"));
}

#[test]
fn a_valid_file_reads_into_its_task_last_updated_at_its_last_event() {
    let (task, mut events) = new_task();
    let text = render_task_md(&TaskDocument::new(&task));
    let file = TaskFile::parse(text.clone().into_bytes(), &task.id, &task.context);
    assert_eq!(file.hash, ContentHash::of(text.as_bytes()));
    assert_eq!(file.text, text);
    let stored = |events: &[EventRecord]| StoredTask {
        id: task.id.clone(),
        context: task.context.clone(),
        location: TaskLocation::Active,
        file: file.clone(),
        events: parse_events(
            &events
                .iter()
                .map(|e| render_event(e).unwrap())
                .collect::<String>(),
        ),
    };
    // Only the creation: updated is the creation time.
    assert_eq!(stored(&events).task(), Ok(task.clone()));
    // A later note moves `updated` to its time.
    let note = EventRecord::new(
        "zzzzzzzz".parse::<EventId>().unwrap(),
        Event::NoteLogged {
            text: "called the client".to_owned(),
        },
        at(250),
        &origin(),
    );
    events.push(note);
    let updated = stored(&events).task().unwrap();
    assert_eq!(updated.updated, at(250));
    assert_eq!(updated.created, at(100));
    // Without any event, the creation time.
    assert_eq!(stored(&[]).task().unwrap().updated, at(100));
}

#[test]
fn a_file_that_is_not_its_task_is_invalid() {
    let (task, _) = new_task();
    let text = render_task_md(&TaskDocument::new(&task));
    let other_id: TaskId = "00000000".parse().unwrap();
    let wrong_id = TaskFile::parse(text.clone().into_bytes(), &other_id, &task.context);
    assert_eq!(
        wrong_id.document.unwrap_err(),
        InvalidTask::WrongId {
            expected: other_id,
            found: task.id.clone(),
        }
    );
    let wrong_context = TaskFile::parse(text.into_bytes(), &task.id, &context("home"));
    assert_eq!(
        wrong_context.document.unwrap_err(),
        InvalidTask::WrongContext {
            expected: context("home"),
            found: context("work"),
        }
    );
}

#[test]
fn a_malformed_or_non_utf8_file_is_invalid_and_keeps_its_hash() {
    let (task, _) = new_task();
    let malformed = TaskFile::parse(b"no frontmatter\n".to_vec(), &task.id, &task.context);
    assert!(matches!(
        malformed.document,
        Err(InvalidTask::Format(ref error)) if error.line() == 1
    ));
    assert_eq!(malformed.hash, ContentHash::of(b"no frontmatter\n"));
    let bytes = b"+++\nid = \"\xff\"\n+++\n".to_vec();
    let binary = TaskFile::parse(bytes.clone(), &task.id, &task.context);
    assert_eq!(binary.document.unwrap_err(), InvalidTask::NotUtf8);
    assert_eq!(binary.hash, ContentHash::of(&bytes));
    // The text is shown, with replacement characters.
    assert!(binary.text.contains('\u{fffd}'));
}

#[test]
fn appended_lines_repair_a_torn_last_line() {
    let (_, events) = new_task();
    let line = render_event(&events[0]).unwrap();
    // Empty file, or one ending with a line ending: the lines as rendered.
    assert_eq!(appended(None, &events).unwrap(), line);
    assert_eq!(appended(Some(b'\n'), &events).unwrap(), line);
    // A last line cut by a crash gets its line ending first: the new line stays whole.
    assert_eq!(appended(Some(b'}'), &events).unwrap(), format!("\n{line}"));
    assert_eq!(appended(Some(b'}'), &[]).unwrap(), "");
    let two = [events[0].clone(), events[0].clone()];
    assert_eq!(appended(None, &two).unwrap(), format!("{line}{line}"));
}

#[test]
fn only_task_id_directory_names_are_tasks() {
    assert_eq!(
        task_dir_id("k3q7zm2a".as_ref()),
        Some("k3q7zm2a".parse().unwrap())
    );
    for name in [
        ".k3q7zm2a.tmp",
        "K3Q7ZM2A",
        "k3q7zm2",
        "notes",
        ".DS_Store",
        "",
    ] {
        assert_eq!(task_dir_id(name.as_ref()), None, "{name}");
    }
}

#[test]
fn only_four_digit_years_are_archive_directories() {
    assert_eq!(archive_year_dir("2026".as_ref()), Some(2026));
    assert_eq!(archive_year_dir("1970".as_ref()), Some(1970));
    for name in [
        "1969", "0000", "+202", "202", "20260", "2026.tmp", ".2026", "abcd", "",
    ] {
        assert_eq!(archive_year_dir(name.as_ref()), None, "{name}");
    }
}
