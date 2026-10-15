//! The change events of `GET /v1/events`: numbered, kept in a bounded buffer for clients that
//! resume, and sent to each connected client through a bounded channel.
//!
//! [`Bus::publish`] never blocks: a client that does not keep up loses its place in the
//! channel, gets [`ChangeEvent::Resync`] and is disconnected.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use futures_util::Stream;
use futures_util::stream;
use loom_api::{ChangeEvent, ContextName, EventCursor};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, watch};

/// Events kept for clients that resume with `Last-Event-ID`.
pub const BUFFER_LEN: usize = 1024;
/// Events a client may fall behind before it is disconnected with `resync`.
pub const CLIENT_CAPACITY: usize = 256;
/// Interval of the heartbeat comment of an idle stream.
pub const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(15);

/// Query parameters of `GET /v1/events`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsQuery {
    /// Only the events of this context; every context when absent or empty.
    pub context: Option<String>,
}

/// The `Last-Event-ID` of a client: `None` when absent or empty. One that is not an id of
/// loomd (a client bug, a proxy) is a resume point no run has, so the client gets `resync`.
#[must_use]
pub fn resume_point(header: Option<&str>) -> Option<EventCursor> {
    let text = header.map(str::trim).filter(|text| !text.is_empty())?;
    Some(text.parse().unwrap_or(EventCursor {
        run: 0,
        seq: u64::MAX,
    }))
}

/// An event and its `id:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Numbered {
    /// Its cursor.
    pub cursor: EventCursor,
    /// The event.
    pub event: ChangeEvent,
}

/// What a client gets first, from the `Last-Event-ID` it sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replay {
    /// No `Last-Event-ID`: only the events to come.
    Nothing,
    /// The buffered events after the resume point.
    From(u64),
    /// The resume point is gone (too old, another run of loomd, a future one): `resync`.
    Resync,
}

/// What to replay to a client that last saw `last`, the buffer holding the events `first..next`
/// of the run `run` (`first == next` when it is empty).
#[must_use]
pub const fn replay(last: Option<EventCursor>, run: u64, first: u64, next: u64) -> Replay {
    let Some(last) = last else {
        return Replay::Nothing;
    };
    // `last.seq + 1` is the first event the client misses; it must still be buffered.
    if last.run == run && last.seq < next && last.seq.saturating_add(1) >= first {
        Replay::From(last.seq + 1)
    } else {
        Replay::Resync
    }
}

/// The events of one loomd run.
#[derive(Debug)]
pub struct Bus {
    run: u64,
    inner: Mutex<Buffer>,
    sender: broadcast::Sender<Numbered>,
    closed: watch::Sender<bool>,
}

#[derive(Debug)]
struct Buffer {
    next: u64,
    events: VecDeque<Numbered>,
    capacity: usize,
}

/// A client's view of the bus: the events to replay first, then the live ones.
#[derive(Debug)]
pub struct Subscription {
    /// Sent before the live events: buffered events, or `resync` alone.
    pub replay: Vec<Numbered>,
    /// The live events, after the last replayed one.
    pub live: broadcast::Receiver<Numbered>,
    /// Becomes `true` when loomd stops: the stream ends.
    pub closed: watch::Receiver<bool>,
}

impl Bus {
    /// A bus for the run `run` (its start, in milliseconds since the Unix epoch), keeping
    /// `buffer` events and letting a client fall `client_capacity` events behind.
    #[must_use]
    pub fn new(run: u64, buffer: usize, client_capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(client_capacity.max(1));
        let (closed, _) = watch::channel(false);
        Self {
            run,
            inner: Mutex::new(Buffer {
                next: 1,
                events: VecDeque::with_capacity(buffer),
                capacity: buffer.max(1),
            }),
            sender,
            closed,
        }
    }

    /// Numbers `event`, buffers it and sends it to the connected clients. Never blocks.
    pub fn publish(&self, event: ChangeEvent) {
        let mut buffer = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let numbered = Numbered {
            cursor: EventCursor {
                run: self.run,
                seq: buffer.next,
            },
            event,
        };
        buffer.next += 1;
        if buffer.events.len() == buffer.capacity {
            buffer.events.pop_front();
        }
        buffer.events.push_back(numbered.clone());
        // Sent under the lock, so that receivers get the events in the order of their ids. No
        // receiver is not an error: nobody listens.
        let _ = self.sender.send(numbered);
        drop(buffer);
    }

    /// Subscribes a client that last saw `last`. The replay and the live events are taken
    /// under one lock: no event is lost or sent twice between them.
    #[must_use]
    pub fn subscribe(&self, last: Option<EventCursor>) -> Subscription {
        let buffer = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let first = buffer
            .events
            .front()
            .map_or(buffer.next, |event| event.cursor.seq);
        let replayed = match replay(last, self.run, first, buffer.next) {
            Replay::Nothing => Vec::new(),
            Replay::From(seq) => buffer
                .events
                .iter()
                .filter(|event| event.cursor.seq >= seq)
                .cloned()
                .collect(),
            Replay::Resync => vec![Numbered {
                // Not numbered: it stands for the events the client cannot get.
                cursor: EventCursor {
                    run: self.run,
                    seq: buffer.next.saturating_sub(1),
                },
                event: ChangeEvent::Resync,
            }],
        };
        // Subscribed under the lock: the first live event follows the last replayed one.
        let live = self.sender.subscribe();
        drop(buffer);
        Subscription {
            replay: replayed,
            live,
            closed: self.closed.subscribe(),
        }
    }

    /// Ends every stream: loomd is stopping, and a graceful shutdown waits for open requests.
    pub fn close(&self) {
        let _ = self.closed.send(true);
    }
}

/// The events a client following `context` (every context when `None`) gets.
///
/// The replay, then the live events, until loomd stops. A client that falls behind gets
/// `resync`, without an id (it may still resume from its last one), and its stream ends.
pub fn stream(
    subscription: Subscription,
    context: Option<ContextName>,
) -> impl Stream<Item = Numbered> {
    struct State {
        replay: VecDeque<Numbered>,
        live: broadcast::Receiver<Numbered>,
        closed: watch::Receiver<bool>,
        context: Option<ContextName>,
        ended: bool,
    }
    let state = State {
        replay: subscription.replay.into(),
        live: subscription.live,
        closed: subscription.closed,
        context,
        ended: false,
    };
    stream::unfold(state, |mut state| async move {
        if state.ended {
            return None;
        }
        while let Some(event) = state.replay.pop_front() {
            if event.event.concerns(state.context.as_ref()) {
                return Some((event, state));
            }
        }
        loop {
            if *state.closed.borrow() {
                return None;
            }
            tokio::select! {
                received = state.live.recv() => match received {
                    Ok(event) if event.event.concerns(state.context.as_ref()) => {
                        return Some((event, state));
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {
                        state.ended = true;
                        let resync = Numbered {
                            cursor: EventCursor { run: 0, seq: 0 },
                            event: ChangeEvent::Resync,
                        };
                        return Some((resync, state));
                    }
                    Err(RecvError::Closed) => return None,
                },
                changed = state.closed.changed() => {
                    if changed.is_err() {
                        return None;
                    }
                }
            }
        }
    })
}

/// Whether `event` carries an id: every one but the `resync` of a client that fell behind.
#[must_use]
pub const fn has_id(event: &Numbered) -> bool {
    event.cursor.run != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN: u64 = 1_791_792_000_000;

    const fn cursor(run: u64, seq: u64) -> EventCursor {
        EventCursor { run, seq }
    }

    fn task(n: u8) -> ChangeEvent {
        ChangeEvent::RepoChanged {
            slug: format!("work/repo{n}"),
        }
    }

    #[test]
    fn a_bad_last_event_id_resyncs() {
        assert_eq!(resume_point(None), None);
        assert_eq!(resume_point(Some(" ")), None);
        assert_eq!(resume_point(Some("12-3")), Some(cursor(12, 3)));
        let bad = resume_point(Some("garbage"));
        assert_eq!(replay(bad, RUN, 1, 10), Replay::Resync);
    }

    #[test]
    fn a_new_client_gets_only_the_events_to_come() {
        assert_eq!(replay(None, RUN, 1, 1), Replay::Nothing);
        assert_eq!(replay(None, RUN, 40, 90), Replay::Nothing);
    }

    #[test]
    fn a_resume_point_still_buffered_replays_what_follows() {
        // Buffer holds 40..90 (next is 90).
        assert_eq!(replay(Some(cursor(RUN, 39)), RUN, 40, 90), Replay::From(40));
        assert_eq!(replay(Some(cursor(RUN, 60)), RUN, 40, 90), Replay::From(61));
        // Up to date: nothing to replay, from the next one.
        assert_eq!(replay(Some(cursor(RUN, 89)), RUN, 40, 90), Replay::From(90));
        // Empty buffer, nothing published yet.
        assert_eq!(replay(Some(cursor(RUN, 0)), RUN, 1, 1), Replay::From(1));
    }

    #[test]
    fn a_resume_point_lost_or_foreign_is_a_resync() {
        assert_eq!(replay(Some(cursor(RUN, 38)), RUN, 40, 90), Replay::Resync);
        assert_eq!(
            replay(Some(cursor(RUN - 1, 60)), RUN, 40, 90),
            Replay::Resync
        );
        assert_eq!(replay(Some(cursor(RUN, 90)), RUN, 40, 90), Replay::Resync);
        assert_eq!(
            replay(Some(cursor(RUN, u64::MAX)), RUN, 40, 90),
            Replay::Resync
        );
    }

    #[test]
    fn the_bus_numbers_buffers_and_replays() {
        let bus = Bus::new(RUN, 3, 8);
        let mut early = bus.subscribe(None);
        for n in 1..=5 {
            bus.publish(task(n));
        }
        // Live: every event, numbered from 1.
        for seq in 1..=5 {
            assert_eq!(early.live.try_recv().unwrap().cursor.seq, seq);
        }
        // The buffer holds the last 3 (3..=5).
        let resumed = bus.subscribe(Some(cursor(RUN, 3)));
        let seqs: Vec<u64> = resumed.replay.iter().map(|e| e.cursor.seq).collect();
        assert_eq!(seqs, [4, 5]);
        let lost = bus.subscribe(Some(cursor(RUN, 1)));
        assert_eq!(lost.replay.len(), 1);
        assert_eq!(lost.replay[0].event, ChangeEvent::Resync);
        // Live events after a subscription follow its replay, none twice.
        let mut late = bus.subscribe(Some(cursor(RUN, 5)));
        assert_eq!(late.replay, Vec::new());
        bus.publish(task(6));
        assert_eq!(late.live.try_recv().unwrap().cursor.seq, 6);
    }

    #[test]
    fn a_client_that_falls_behind_is_told_it_lagged() {
        let bus = Bus::new(RUN, 16, 2);
        let mut slow = bus.subscribe(None);
        for n in 1..=5 {
            bus.publish(task(n));
        }
        assert!(matches!(
            slow.live.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
    }

    fn collect(bus: &Bus, last: Option<EventCursor>, context: Option<&str>) -> Vec<Numbered> {
        use futures_util::StreamExt as _;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let subscription = bus.subscribe(last);
        bus.close();
        runtime.block_on(
            stream(subscription, context.map(|name| name.parse().unwrap())).collect::<Vec<_>>(),
        )
    }

    #[test]
    fn the_stream_filters_by_context_and_ends_on_close() {
        let bus = Bus::new(RUN, 16, 16);
        bus.publish(task(1));
        bus.publish(ChangeEvent::RepoChanged {
            slug: "home/notes".to_owned(),
        });
        bus.publish(task(2));
        let work: Vec<u64> = collect(&bus, Some(cursor(RUN, 0)), Some("work"))
            .iter()
            .map(|event| event.cursor.seq)
            .collect();
        assert_eq!(work, [1, 3]);
    }

    #[test]
    fn a_lagging_stream_ends_with_resync() {
        use futures_util::StreamExt as _;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let bus = Bus::new(RUN, 16, 2);
        let subscription = bus.subscribe(None);
        for n in 1..=5 {
            bus.publish(task(n));
        }
        let events: Vec<Numbered> = runtime.block_on(stream(subscription, None).collect());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, ChangeEvent::Resync);
        assert!(!has_id(&events[0]));
    }

    #[test]
    fn close_ends_the_streams() {
        let bus = Bus::new(RUN, 4, 4);
        let subscription = bus.subscribe(None);
        assert!(!*subscription.closed.borrow());
        bus.close();
        assert!(*subscription.closed.borrow());
    }
}
