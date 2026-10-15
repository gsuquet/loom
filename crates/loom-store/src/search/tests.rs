use loom_api::{RepoMatch, SearchField, SearchKind, SearchResult, TaskMatch, TaskSearchField};
use loom_core::StatusName;

use super::{
    Expressions, Group, RepoCandidate, TaskCandidate, TaskTier, Tier, covers_whole, expressions,
    is_key, merge, rank_repos, rank_tasks, status_name, terms,
};

fn candidate(slug: &str, tier: Tier, last_used: Option<i64>, bm25: f64) -> RepoCandidate {
    RepoCandidate {
        result: RepoMatch {
            slug: slug.to_owned(),
            name: slug.rsplit('/').next().unwrap_or(slug).to_owned(),
            context: "p".to_owned(),
            path: format!("/w/{slug}").into(),
            score: -bm25,
            matched_fields: vec![SearchField::Name],
        },
        tier,
        last_used,
        bm25,
    }
}

/// A task candidate; `id` is 8 characters of the ID alphabet.
fn task(id: &str, group: Group, tier: TaskTier, updated: Option<i64>, bm25: f64) -> TaskCandidate {
    TaskCandidate {
        result: TaskMatch {
            id: id.parse().unwrap(),
            context: "work".parse().unwrap(),
            title: format!("task {id}"),
            status: Some("ready".to_owned()),
            link_keys: Vec::new(),
            score: -bm25,
            matched_fields: vec![TaskSearchField::Title],
        },
        group,
        tier,
        updated,
        bm25,
    }
}

fn ranked(mut candidates: Vec<RepoCandidate>) -> Vec<String> {
    rank_repos(&mut candidates);
    candidates
        .into_iter()
        .map(|candidate| candidate.result.slug)
        .collect()
}

fn ranked_tasks(mut candidates: Vec<TaskCandidate>) -> Vec<String> {
    rank_tasks(&mut candidates);
    candidates
        .into_iter()
        .map(|candidate| candidate.result.id.to_string())
        .collect()
}

/// `repo:<slug>` or `task:<id>`, in order.
fn labels(results: &[SearchResult]) -> Vec<String> {
    results
        .iter()
        .map(|result| match result {
            SearchResult::Repo(repo) => format!("repo:{}", repo.slug),
            SearchResult::Task(task) => format!("task:{}", task.id),
        })
        .collect()
}

#[test]
fn ranking_table() {
    use Tier::{ExactName, NamePrefix, Other, Slug};
    let cases: Vec<(&str, Vec<RepoCandidate>, Vec<&str>)> = vec![
        (
            "tier first, whatever the recency and relevance",
            vec![
                candidate("p/other", Other, Some(900), -9.0),
                candidate("p/slug", Slug, Some(800), -8.0),
                candidate("p/prefix", NamePrefix, Some(700), -7.0),
                candidate("p/exact", ExactName, None, -1.0),
            ],
            vec!["p/exact", "p/prefix", "p/slug", "p/other"],
        ),
        (
            "same tier: most recently used first, never used last",
            vec![
                candidate("p/never", NamePrefix, None, -9.0),
                candidate("p/old", NamePrefix, Some(10), -1.0),
                candidate("p/new", NamePrefix, Some(20), -1.0),
            ],
            vec!["p/new", "p/old", "p/never"],
        ),
        (
            "same tier and recency: lower bm25 first",
            vec![
                candidate("p/a", Other, Some(5), -1.0),
                candidate("p/b", Other, Some(5), -3.0),
                candidate("p/c", Other, Some(5), -2.0),
            ],
            vec!["p/b", "p/c", "p/a"],
        ),
        (
            "full tie: by slug",
            vec![
                candidate("p/z", Slug, None, -1.0),
                candidate("p/a", Slug, None, -1.0),
                candidate("o/m", Slug, None, -1.0),
            ],
            vec!["o/m", "p/a", "p/z"],
        ),
        ("nothing", vec![], vec![]),
    ];
    for (name, candidates, expected) in cases {
        assert_eq!(ranked(candidates), expected, "{name}");
    }
}

#[test]
fn ranking_is_stable_under_permutation() {
    let all = || {
        vec![
            candidate("p/a", Tier::Other, None, -1.0),
            candidate("p/b", Tier::Other, Some(3), -1.0),
            candidate("p/c", Tier::Slug, None, -2.0),
            candidate("p/d", Tier::ExactName, Some(1), -0.5),
        ]
    };
    let expected = ranked(all());
    let mut reversed = all();
    reversed.reverse();
    assert_eq!(ranked(reversed), expected);
    assert_eq!(expected, ["p/d", "p/c", "p/b", "p/a"]);
}

#[test]
fn tier_table() {
    let cases = [
        ((true, true, true), Tier::ExactName),
        ((true, false, false), Tier::ExactName),
        ((false, true, true), Tier::NamePrefix),
        ((false, true, false), Tier::NamePrefix),
        ((false, false, true), Tier::Slug),
        ((false, false, false), Tier::Other),
    ];
    for ((exact, name, slug), expected) in cases {
        assert_eq!(
            Tier::of(exact, name, slug),
            expected,
            "{exact} {name} {slug}"
        );
    }
}

#[test]
fn groups_come_in_order_and_follow_the_status() {
    use Group::{Archived, Closed, Current, Inbox, Repo};
    let mut groups = [Archived, Closed, Inbox, Repo, Current];
    groups.sort();
    assert_eq!(groups, [Current, Repo, Inbox, Closed, Archived]);

    let cases = [
        (Some(StatusName::Active), Current),
        (Some(StatusName::Ready), Current),
        (Some(StatusName::Waiting), Current),
        (Some(StatusName::Inbox), Inbox),
        (Some(StatusName::Done), Closed),
        (Some(StatusName::Cancelled), Closed),
        // A status of a newer loom, or an invalid file.
        (None, Inbox),
    ];
    for (status, expected) in cases {
        assert_eq!(Group::of_task(status, false), expected, "{status:?}");
        // Archived tasks come last, whatever their status.
        assert_eq!(Group::of_task(status, true), Archived, "{status:?}");
    }
}

#[test]
fn status_names_are_read_as_written() {
    for name in StatusName::ALL {
        assert_eq!(status_name(&name.to_string()), Some(name));
    }
    assert_eq!(status_name("someday"), None);
    assert_eq!(status_name("Active"), None);
    assert_eq!(status_name(""), None);
}

#[test]
fn task_tier_table() {
    let cases = [
        ((true, true), TaskTier::Key),
        ((true, false), TaskTier::Key),
        ((false, true), TaskTier::Title),
        ((false, false), TaskTier::Other),
    ];
    for ((key, title), expected) in cases {
        assert_eq!(TaskTier::of(key, title), expected, "{key} {title}");
    }
}

#[test]
fn a_key_is_the_whole_query_case_ignored() {
    let keys = vec!["CORE-1042".to_owned(), "!88".to_owned()];
    assert!(is_key(&["CORE-1042"], &keys));
    assert!(is_key(&["core-1042"], &keys));
    assert!(is_key(&["!88"], &keys));
    assert!(!is_key(&["CORE"], &keys));
    assert!(!is_key(&["1042"], &keys));
    assert!(!is_key(&["88"], &keys));
    assert!(!is_key(&["CORE-1042", "timeout"], &keys));
    assert!(!is_key(&[], &keys));
    assert!(!is_key(&["CORE-1042"], &[]));
}

#[test]
fn task_ranking_table() {
    use Group::{Closed, Current, Inbox};
    use TaskTier::{Key, Other, Title};
    let cases: Vec<(&str, Vec<TaskCandidate>, Vec<&str>)> = vec![
        (
            "group first, whatever the tier",
            vec![
                task("cccccccc", Closed, Key, Some(9), -9.0),
                task("bbbbbbbb", Inbox, Key, Some(9), -9.0),
                task("aaaaaaaa", Current, Other, None, -1.0),
            ],
            vec!["aaaaaaaa", "bbbbbbbb", "cccccccc"],
        ),
        (
            "same group: key, then title, then the other fields",
            vec![
                task("aaaaaaaa", Current, Other, Some(9), -9.0),
                task("bbbbbbbb", Current, Title, Some(8), -8.0),
                task("cccccccc", Current, Key, None, -1.0),
            ],
            vec!["cccccccc", "bbbbbbbb", "aaaaaaaa"],
        ),
        (
            "same tier: most recently changed first, invalid files last",
            vec![
                task("aaaaaaaa", Inbox, Other, None, -9.0),
                task("bbbbbbbb", Inbox, Other, Some(10), -1.0),
                task("cccccccc", Inbox, Other, Some(20), -1.0),
            ],
            vec!["cccccccc", "bbbbbbbb", "aaaaaaaa"],
        ),
        (
            "same recency: lower bm25 first, then by ID",
            vec![
                task("dddddddd", Current, Title, Some(5), -1.0),
                task("cccccccc", Current, Title, Some(5), -1.0),
                task("bbbbbbbb", Current, Title, Some(5), -3.0),
            ],
            vec!["bbbbbbbb", "cccccccc", "dddddddd"],
        ),
        ("nothing", vec![], vec![]),
    ];
    for (name, candidates, expected) in cases {
        assert_eq!(ranked_tasks(candidates), expected, "{name}");
    }
}

#[test]
fn merge_puts_current_tasks_then_repos_then_inbox_then_closed_tasks() {
    let repos = vec![
        candidate("p/other", Tier::Other, None, -1.0),
        candidate("p/exact", Tier::ExactName, None, -1.0),
    ];
    let tasks = vec![
        task("aaaaaaaa", Group::Closed, TaskTier::Key, Some(9), -9.0),
        task("bbbbbbbb", Group::Inbox, TaskTier::Title, Some(1), -1.0),
        task("cccccccc", Group::Current, TaskTier::Other, Some(1), -1.0),
        task("dddddddd", Group::Current, TaskTier::Key, Some(1), -1.0),
    ];
    let all = merge(repos.clone(), tasks.clone(), 100);
    assert_eq!(
        labels(&all),
        [
            "task:dddddddd",
            "task:cccccccc",
            "repo:p/exact",
            "repo:p/other",
            "task:bbbbbbbb",
            "task:aaaaaaaa",
        ]
    );
    assert_eq!(all[2].kind(), SearchKind::Repo);

    // The limit cuts the merged list, not each kind.
    assert_eq!(
        labels(&merge(repos.clone(), tasks.clone(), 3)),
        ["task:dddddddd", "task:cccccccc", "repo:p/exact"]
    );
    assert_eq!(merge(repos.clone(), tasks, 0), []);
    assert_eq!(
        labels(&merge(repos, Vec::new(), 100)),
        ["repo:p/exact", "repo:p/other"]
    );
}

#[test]
fn archived_tasks_come_last() {
    let all = merge(
        vec![candidate("p/x", Tier::Other, None, -1.0)],
        vec![
            task("aaaaaaaa", Group::Archived, TaskTier::Key, Some(9), -9.0),
            task("bbbbbbbb", Group::Closed, TaskTier::Other, None, -1.0),
        ],
        100,
    );
    assert_eq!(labels(&all), ["repo:p/x", "task:bbbbbbbb", "task:aaaaaaaa"]);
}

#[test]
fn merge_is_stable_under_permutation() {
    let repos = || {
        vec![
            candidate("p/a", Tier::Other, None, -1.0),
            candidate("p/b", Tier::Slug, Some(1), -1.0),
        ]
    };
    let tasks = || {
        vec![
            task("aaaaaaaa", Group::Inbox, TaskTier::Other, Some(1), -1.0),
            task("bbbbbbbb", Group::Current, TaskTier::Other, Some(1), -1.0),
            task("cccccccc", Group::Current, TaskTier::Other, Some(1), -1.0),
        ]
    };
    let expected = labels(&merge(repos(), tasks(), 100));
    let (mut reversed_repos, mut reversed_tasks) = (repos(), tasks());
    reversed_repos.reverse();
    reversed_tasks.reverse();
    assert_eq!(
        labels(&merge(reversed_repos, reversed_tasks, 100)),
        expected
    );
}

#[test]
fn terms_split_on_whitespace_and_drop_punctuation_only_words() {
    assert_eq!(terms("  loom\tapi \n"), ["loom", "api"]);
    assert_eq!(terms("- loom -- * \"\""), ["loom"]);
    assert_eq!(terms("café 42"), ["café", "42"]);
    assert_eq!(terms("CORE-1042 !88"), ["CORE-1042", "!88"]);
    assert_eq!(terms(""), [] as [&str; 0]);
    assert_eq!(terms("  "), [] as [&str; 0]);
}

#[test]
fn expressions_quote_every_term() {
    assert_eq!(expressions(&[]), None);
    assert_eq!(
        expressions(&["lo", "api"]),
        Some(Expressions {
            all: r#""lo"* AND "api"*"#.to_owned(),
            any: r#""lo"* OR "api"*"#.to_owned(),
            name: r#"{name} : ("lo"* AND "api"*)"#.to_owned(),
            slug: r#"{slug} : ("lo"* AND "api"*)"#.to_owned(),
            exact_name: r#"{name} : ^"lo api""#.to_owned(),
        })
    );
    assert_eq!(
        Expressions::in_column("link_keys", r#""!88"*"#),
        r#"{link_keys} : ("!88"*)"#
    );
    // FTS5 syntax stays inside quotes; inner quotes are doubled.
    let syntax = expressions(&["NEAR(a", "b\"c", "x:y", "-z", "^w", "OR"]).unwrap();
    assert_eq!(
        syntax.all,
        r#""NEAR(a"* AND "b""c"* AND "x:y"* AND "-z"* AND "^w"* AND "OR"*"#
    );
}

#[test]
fn covers_whole_needs_every_letter_and_digit_inside_a_match() {
    assert!(covers_whole("\u{1}loom\u{2}"));
    assert!(covers_whole("\u{1}loom-api\u{2}"));
    assert!(covers_whole("\u{1}loom\u{2}-\u{1}api\u{2}."));
    assert!(!covers_whole("\u{1}loom\u{2}-api"));
    assert!(!covers_whole("x\u{1}loom\u{2}"));
    assert!(!covers_whole("loom"));
}
