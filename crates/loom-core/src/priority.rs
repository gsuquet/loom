//! The priority list: which open tasks are listed, in which order, and why.
//!
//! The order is lexicographic, without weights (ADR-0013, decision 3).

use std::cmp::Ordering;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Date;
use crate::id::TaskId;
use crate::task::{Status, Task};

/// A task of the priority list, at its position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Ranked {
    /// The ranked task.
    pub task_id: TaskId,
    /// Position in the list, from 1.
    pub position: usize,
    /// The rule that placed the task after its predecessor.
    pub reason: RankReason,
}

/// The rule that placed a task after its predecessor in the priority list, for `--why`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RankReason {
    /// The task heads the list: it has no predecessor.
    First,
    /// The predecessor is pinned, this task is not.
    Pinned,
    /// The predecessor is overdue or due today and this task is not, or both are and the
    /// predecessor is due earlier.
    Urgent,
    /// The predecessor is more important.
    Importance,
    /// The predecessor is due earlier, or this task has no due date.
    DueDate,
    /// The predecessor is older.
    Age,
    /// Every other rule ties: the predecessor has the smaller ID.
    Id,
}

/// The priority list of `tasks` on `today`.
///
/// Lists `ready` and `active` tasks, and `waiting` tasks whose `until` is today or past.
/// `inbox`, `done`, `cancelled` and unknown statuses are never listed; a task of an unknown kind
/// is listed by its status.
///
/// Order, each rule breaking the ties of the previous one:
/// 1. pinned first;
/// 2. overdue or due today first, earliest due date first;
/// 3. importance, p0 first;
/// 4. due date, earliest first, none last;
/// 5. age, oldest `created` first;
/// 6. ID, so that the order is total and does not depend on the order of `tasks`.
#[must_use]
pub fn rank(tasks: &[Task], today: Date) -> Vec<Ranked> {
    let mut listed: Vec<&Task> = tasks.iter().filter(|task| is_listed(task, today)).collect();
    listed.sort_by(|a, b| compare(a, b, today).0);
    let mut ranked = Vec::with_capacity(listed.len());
    let mut predecessor: Option<&Task> = None;
    for (index, task) in listed.into_iter().enumerate() {
        let reason = predecessor.map_or(RankReason::First, |previous| {
            compare(previous, task, today).1
        });
        ranked.push(Ranked {
            task_id: task.id.clone(),
            position: index + 1,
            reason,
        });
        predecessor = Some(task);
    }
    ranked
}

/// Whether `task` is in the priority list on `today`.
fn is_listed(task: &Task, today: Date) -> bool {
    match &task.status {
        Status::Ready | Status::Active => true,
        // Hidden while `until` is unset or in the future; on the `until` date the task shows
        // again.
        Status::Waiting { until, .. } => until.is_some_and(|until| until <= today),
        // Unknown: written by a newer version, its meaning for the list is unknown.
        Status::Inbox | Status::Done | Status::Cancelled | Status::Unknown(_) => false,
    }
}

/// The order of `a` and `b`, and the first rule that is not a tie (`Id` when all are).
fn compare(a: &Task, b: &Task, today: Date) -> (Ordering, RankReason) {
    let urgent_due = |task: &Task| task.due.filter(|due| *due <= today);
    let rules = [
        (b.pinned.cmp(&a.pinned), RankReason::Pinned),
        (
            compare_none_last(urgent_due(a), urgent_due(b)),
            RankReason::Urgent,
        ),
        (a.importance.cmp(&b.importance), RankReason::Importance),
        (compare_none_last(a.due, b.due), RankReason::DueDate),
        (a.created.cmp(&b.created), RankReason::Age),
    ];
    rules
        .into_iter()
        .find(|(ordering, _)| ordering.is_ne())
        .unwrap_or_else(|| (a.id.cmp(&b.id), RankReason::Id))
}

/// Earliest first, `None` last.
fn compare_none_last(a: Option<Date>, b: Option<Date>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ContextName;
    use crate::task::{Importance, Kind, Relations};
    use crate::timestamp::Timestamp;

    /// Wednesday 2026-10-14.
    fn today() -> Date {
        "2026-10-14".parse().unwrap()
    }

    fn day(offset: i32) -> Date {
        today().checked_add(jiff::Span::new().days(offset)).unwrap()
    }

    /// A ready p2 task created at `n` seconds, with ID `aaaaaa` followed by two characters of `n`.
    fn task(n: u64) -> Task {
        const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
        let suffix: String = [n / 32 % 32, n % 32]
            .iter()
            .map(|&index| char::from(ALPHABET[usize::try_from(index).unwrap()]))
            .collect();
        Task {
            id: format!("aaaaaa{suffix}").parse().unwrap(),
            title: format!("task {n}"),
            kind: Kind::Chore,
            context: "company-a".parse::<ContextName>().unwrap(),
            status: Status::Ready,
            importance: Importance::P2,
            due: None,
            pinned: false,
            links: Vec::new(),
            relations: Relations::default(),
            created: Timestamp::from_unix_secs(n).unwrap(),
            updated: Timestamp::from_unix_secs(n).unwrap(),
        }
    }

    fn ids(ranked: &[Ranked]) -> Vec<TaskId> {
        ranked.iter().map(|ranked| ranked.task_id.clone()).collect()
    }

    fn reasons(ranked: &[Ranked]) -> Vec<RankReason> {
        ranked.iter().map(|ranked| ranked.reason).collect()
    }

    /// Ranks `[a, b]` in both input orders, checks `a` comes first, and returns the reason of `b`.
    fn a_before_b(a: &Task, b: &Task) -> RankReason {
        for input in [vec![a.clone(), b.clone()], vec![b.clone(), a.clone()]] {
            let ranked = rank(&input, today());
            assert_eq!(ids(&ranked), vec![a.id.clone(), b.id.clone()]);
        }
        let ranked = rank(&[a.clone(), b.clone()], today());
        assert_eq!(ranked[0].reason, RankReason::First);
        ranked[1].reason
    }

    #[test]
    fn positions_count_from_one() {
        let ranked = rank(&[task(1), task(2), task(3)], today());
        let positions: Vec<usize> = ranked.iter().map(|ranked| ranked.position).collect();
        assert_eq!(positions, vec![1, 2, 3]);
    }

    #[test]
    fn an_empty_list_ranks_nothing() {
        assert_eq!(rank(&[], today()), Vec::new());
    }

    #[test]
    fn a_pinned_p3_task_ranks_above_an_overdue_p0_task() {
        let pinned = Task {
            pinned: true,
            importance: Importance::P3,
            ..task(2)
        };
        let overdue = Task {
            importance: Importance::P0,
            due: Some(day(-3)),
            ..task(1)
        };
        assert_eq!(a_before_b(&pinned, &overdue), RankReason::Pinned);
    }

    #[test]
    fn pinned_tasks_are_ordered_by_the_next_rules() {
        let urgent = Task {
            pinned: true,
            due: Some(day(0)),
            ..task(2)
        };
        let later = Task {
            pinned: true,
            due: Some(day(1)),
            ..task(1)
        };
        assert_eq!(a_before_b(&urgent, &later), RankReason::Urgent);
    }

    #[test]
    fn a_task_due_today_ranks_above_a_p0_task_due_tomorrow() {
        let due_today = Task {
            importance: Importance::P3,
            due: Some(day(0)),
            ..task(2)
        };
        let p0_tomorrow = Task {
            importance: Importance::P0,
            due: Some(day(1)),
            ..task(1)
        };
        assert_eq!(a_before_b(&due_today, &p0_tomorrow), RankReason::Urgent);
    }

    #[test]
    fn an_overdue_task_ranks_above_a_p0_task_without_due_date() {
        let overdue = Task {
            importance: Importance::P3,
            due: Some(day(-1)),
            ..task(2)
        };
        let p0 = Task {
            importance: Importance::P0,
            ..task(1)
        };
        assert_eq!(a_before_b(&overdue, &p0), RankReason::Urgent);
    }

    #[test]
    fn urgent_tasks_come_earliest_due_first_whatever_their_importance() {
        let older_due = Task {
            importance: Importance::P3,
            due: Some(day(-2)),
            ..task(2)
        };
        let due_today = Task {
            importance: Importance::P0,
            due: Some(day(0)),
            ..task(1)
        };
        assert_eq!(a_before_b(&older_due, &due_today), RankReason::Urgent);
    }

    #[test]
    fn urgent_tasks_due_the_same_day_are_ordered_by_importance() {
        let p0 = Task {
            importance: Importance::P0,
            due: Some(day(-1)),
            ..task(2)
        };
        let p1 = Task {
            importance: Importance::P1,
            due: Some(day(-1)),
            ..task(1)
        };
        assert_eq!(a_before_b(&p0, &p1), RankReason::Importance);
    }

    #[test]
    fn importance_comes_before_a_future_due_date() {
        let p1 = Task {
            importance: Importance::P1,
            ..task(2)
        };
        let p2_tomorrow = Task {
            importance: Importance::P2,
            due: Some(day(1)),
            ..task(1)
        };
        assert_eq!(a_before_b(&p1, &p2_tomorrow), RankReason::Importance);
    }

    #[test]
    fn at_equal_importance_the_earlier_future_due_date_comes_first() {
        let tomorrow = Task {
            due: Some(day(1)),
            ..task(2)
        };
        let next_week = Task {
            due: Some(day(7)),
            ..task(1)
        };
        assert_eq!(a_before_b(&tomorrow, &next_week), RankReason::DueDate);
    }

    #[test]
    fn a_task_without_due_date_comes_after_one_with_a_future_due_date() {
        let next_year = Task {
            due: Some(day(365)),
            ..task(2)
        };
        let undated = task(1);
        assert_eq!(a_before_b(&next_year, &undated), RankReason::DueDate);
    }

    #[test]
    fn at_equal_due_date_the_older_task_comes_first() {
        let older = Task {
            due: Some(day(1)),
            ..task(1)
        };
        let newer = Task {
            due: Some(day(1)),
            ..task(2)
        };
        assert_eq!(a_before_b(&older, &newer), RankReason::Age);
        assert_eq!(a_before_b(&task(1), &task(2)), RankReason::Age);
    }

    #[test]
    fn the_id_breaks_the_last_tie() {
        let first = task(1);
        let second = Task {
            created: first.created,
            ..task(2)
        };
        assert_eq!(a_before_b(&first, &second), RankReason::Id);
    }

    #[test]
    fn only_ready_active_and_due_waiting_tasks_are_ranked() {
        let cases = [
            (Status::Inbox, false),
            (Status::Ready, true),
            (Status::Active, true),
            (Status::Done, false),
            (Status::Cancelled, false),
            (Status::Unknown("snoozed".to_owned()), false),
        ];
        for (status, listed) in cases {
            let ranked = rank(
                &[Task {
                    status: status.clone(),
                    ..task(1)
                }],
                today(),
            );
            assert_eq!(ranked.len(), usize::from(listed), "{status:?}");
        }
    }

    #[test]
    fn a_waiting_task_is_hidden_until_its_until_date() {
        let cases = [
            (Some(day(-1)), true),
            (Some(day(0)), true),
            (Some(day(1)), false),
            (None, false),
        ];
        for (until, listed) in cases {
            let waiting = Task {
                status: Status::Waiting {
                    on: Some("Alice".to_owned()),
                    until,
                },
                ..task(1)
            };
            let ranked = rank(&[waiting], today());
            assert_eq!(ranked.len(), usize::from(listed), "until {until:?}");
        }
    }

    #[test]
    fn a_task_of_an_unknown_kind_is_ranked_by_its_status() {
        let unknown = Task {
            kind: Kind::Unknown {
                name: "meeting".to_owned(),
                format: None,
            },
            ..task(1)
        };
        assert_eq!(
            ids(&rank(std::slice::from_ref(&unknown), today())),
            vec![unknown.id]
        );
    }

    #[test]
    fn reasons_name_the_rule_against_the_predecessor() {
        let pinned = Task {
            pinned: true,
            ..task(9)
        };
        let overdue = Task {
            due: Some(day(-1)),
            ..task(8)
        };
        let p0 = Task {
            importance: Importance::P0,
            ..task(7)
        };
        let tomorrow = Task {
            due: Some(day(1)),
            ..task(6)
        };
        let old = task(2);
        let new = task(3);
        let twin = Task {
            created: new.created,
            ..task(4)
        };
        let ranked = rank(
            &[
                twin.clone(),
                old.clone(),
                new.clone(),
                tomorrow.clone(),
                p0.clone(),
                overdue.clone(),
                pinned.clone(),
            ],
            today(),
        );
        assert_eq!(
            ids(&ranked),
            vec![
                pinned.id,
                overdue.id,
                p0.id,
                tomorrow.id,
                old.id,
                new.id,
                twin.id
            ]
        );
        assert_eq!(
            reasons(&ranked),
            vec![
                RankReason::First,
                RankReason::Pinned,
                RankReason::Urgent,
                RankReason::Importance,
                RankReason::DueDate,
                RankReason::Age,
                RankReason::Id,
            ]
        );
    }

    /// Every combination of the ranking inputs over small sets, with distinct IDs.
    fn exhaustive_tasks() -> Vec<Task> {
        let mut tasks = Vec::new();
        let mut n = 0;
        for pinned in [false, true] {
            for due in [
                None,
                Some(day(-1)),
                Some(day(0)),
                Some(day(1)),
                Some(day(2)),
            ] {
                for importance in [Importance::P0, Importance::P3] {
                    for created in [1, 2] {
                        n += 1;
                        tasks.push(Task {
                            pinned,
                            due,
                            importance,
                            created: Timestamp::from_unix_secs(created).unwrap(),
                            ..task(n)
                        });
                    }
                }
            }
        }
        tasks
    }

    #[test]
    fn the_order_is_a_strict_total_order_over_distinct_ids() {
        let tasks = exhaustive_tasks();
        for a in &tasks {
            assert_eq!(compare(a, a, today()).0, Ordering::Equal);
            for b in &tasks {
                let (forward, reason) = compare(a, b, today());
                let (backward, reason_back) = compare(b, a, today());
                assert_eq!(forward, backward.reverse(), "antisymmetry");
                assert_eq!(reason, reason_back, "the deciding rule is symmetric");
                if a.id != b.id {
                    assert_ne!(forward, Ordering::Equal, "totality");
                }
                for c in &tasks {
                    if forward == Ordering::Less && compare(b, c, today()).0 == Ordering::Less {
                        assert_eq!(compare(a, c, today()).0, Ordering::Less, "transitivity");
                    }
                }
            }
        }
    }

    #[test]
    fn the_order_does_not_depend_on_the_input_order() {
        let tasks = exhaustive_tasks();
        let expected = rank(&tasks, today());
        assert_eq!(expected.len(), tasks.len());
        let mut reversed = tasks.clone();
        reversed.reverse();
        assert_eq!(rank(&reversed, today()), expected);
        for shift in 1..tasks.len() {
            let mut rotated = tasks.clone();
            rotated.rotate_left(shift);
            assert_eq!(rank(&rotated, today()), expected);
        }
        for pair in expected.windows(2) {
            assert_ne!(pair[1].reason, RankReason::First);
        }
    }
}
