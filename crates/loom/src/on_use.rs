//! Registration on use: a `loom` command run inside a repository tells loomd, which registers
//! the repository when its index does not have it and marks it used. One request on a thread
//! of its own, never starting loomd, never failing the command, and never delaying it by more
//! than [`BUDGET`].

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{RegisterRequest, RepoSummary};
use loom_config::layout::Layout;
use loom_repo::find_root;
use tracing::{Instrument as _, Span, debug};

use crate::client::Client;
use crate::repo_cmd::{REGISTER_PATH, logical_cwd};

/// Longest a command waits for the registration, counted from its start.
pub const BUDGET: Duration = Duration::from_millis(50);

/// A registration in flight.
pub struct Pending {
    done: mpsc::Receiver<()>,
    deadline: Instant,
}

impl Pending {
    /// Waits for the registration until [`BUDGET`] after it started, not longer. A request
    /// still in flight is abandoned when the process exits.
    pub fn wait(self) {
        let left = self.deadline.saturating_duration_since(Instant::now());
        let _ = self.done.recv_timeout(left);
    }
}

/// Starts the registration of the repository of the current directory, when loomd's socket
/// exists. Everything that fails is logged at debug level and skipped.
pub fn spawn(layout: Option<Layout>) -> Option<Pending> {
    let started = Instant::now();
    let layout = layout?;
    let parent = Span::current();
    let (sender, done) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("register-on-use".to_owned())
        .spawn(move || {
            register(&layout, started, parent);
            let _ = sender.send(());
        });
    match spawned {
        Ok(_) => Some(Pending {
            done,
            deadline: started + BUDGET,
        }),
        Err(error) => {
            debug!(%error, "cannot start the registration on use");
            None
        }
    }
}

fn register(layout: &Layout, started: Instant, parent: Span) {
    // No socket: loomd is not running, and it is never started for this.
    if fs::symlink_metadata(&layout.socket).is_err() {
        debug!("loomd is not running: no registration on use");
        return;
    }
    let root: Option<PathBuf> = logical_cwd()
        .ok()
        .and_then(|cwd| find_root(&cwd).ok().flatten());
    let Some(root) = root else {
        return;
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let left = (started + BUDGET).saturating_duration_since(Instant::now());
    let client = Client::new(layout.socket.clone()).with_timeout(left);
    let request = RegisterRequest {
        path: root,
        on_use: true,
    };
    let sent = client
        .post::<RepoSummary>(REGISTER_PATH, Some(&request))
        .instrument(parent);
    match runtime.block_on(sent) {
        Ok(repo) => debug!(slug = %repo.identity.slug, "repository marked used"),
        Err(error) => debug!(%error, "registration on use skipped"),
    }
}
