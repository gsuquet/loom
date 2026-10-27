//! MR review (ADR-0016): the provider-neutral model of an MR, the parsers of the GitHub and
//! GitLab payloads, MR keys and the context of an MR.
//!
//! Every module is pure (no I/O, no clock, no async runtime), except [`client`], the HTTP
//! clients of the forges.

pub mod client;
mod context;
mod error;
pub mod github;
pub mod gitlab;
mod key;
pub mod model;

pub use context::{MrContext, MrContexts, mr_context};
pub use error::{ErrorBody, GraphqlError, ParseError};

use model::Thread;

/// Sorts threads in one order for both forges: by the time of their first comment, then by
/// ID. Pure.
pub fn sort_threads(threads: &mut [Thread]) {
    threads.sort_by(|a, b| {
        let time = |thread: &Thread| thread.comments.first().map(|comment| comment.created_at);
        time(a).cmp(&time(b)).then_with(|| a.id.cmp(&b.id))
    });
}
