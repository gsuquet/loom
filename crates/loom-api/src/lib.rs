//! Types shared by the loomd API, the CLI `--json` output and the MCP tools.
//!
//! No I/O, no clock, no async runtime. Request types use `#[serde(deny_unknown_fields)]`;
//! response types accept unknown fields so that an older client can read a newer daemon.

mod api_version;
mod auto_update;
mod backup;
mod context;
mod disk;
mod doctor;
mod error;
mod events;
mod repo;
mod repo_state;
mod repo_update;
mod review;
mod search;
mod service;
mod status;
mod task;
mod task_index;
mod update;
mod version;

pub use api_version::ApiVersion;
pub use auto_update::{
    ChannelSource, ChosenChannel, PendingUpgrade, UpdateChannel, UpdatePolicyMode, UpdateResult,
    UpdateState, UpdateStatus,
};
pub use backup::{
    BackupOutcome, BackupReport, BackupRequest, BackupStep, BackupSync, ContextBackup,
};
pub use context::ContextAssignment;
pub use disk::{ContextDiskUsage, DiskUsage};
pub use doctor::{
    CheckStatus, CredentialOrigin, DaemonDiagnostics, DoctorCheck, DoctorReport, ForgeAccess,
    ForgeCheck, ForgeCheckOutcome, ForgeCheckResult, RemoteCheck, RemoteCheckOutcome,
    RemoteCheckResult, RemoteProblem, RemoteScheme, SshAgentSocket, TokenProblem, TokenSourceKind,
};
pub use error::{ApiError, ErrorCode};
// Moved to loom-core, which loom-api builds on; re-exported so that clients keep their paths.
pub use loom_core::{
    CONTEXT_NAME_MAX_LEN, ContextName, ContextNameError, TaskId, Timestamp, UNASSIGNED,
};
// The task domain, as the task endpoints send and receive it.
pub use events::{ChangeEvent, EventCursor, EventCursorError};
pub use loom_core::{
    Action, Actor, Change, ContentFormat, Date, Event, EventId, EventRecord, ExternalRef,
    FieldChange, Importance, Kind, Link, MachineId, NewTask, NotesHash, RankReason, Relations,
    SkippedLine, Status, StatusName, Task, TaskEdit,
};
pub use repo::{
    ForgeKind, RegisterRequest, Remote, RemoteUrl, RepoIdentity, RepoList, RepoSummary,
    RepoUpdatePolicy, ScanIssue, ScanResult,
};
pub use repo_state::{MaybeKnown, RepoHead, RepoOperation, RepoState, Untracked, Upstream};
pub use repo_update::{
    LastUpdate, PlannedAction, RepoSync, SkipReason, StateField, SyncOutcome, SyncRequest,
    SyncResult, UpdateOutcome, UpdateStep,
};
pub use review::{
    ReviewSync, ReviewSyncFailure, ReviewSyncOutcome, ReviewSyncRequest, ReviewSyncResult,
};
pub use search::{
    DEFAULT_SEARCH_LIMIT, RepoMatch, SearchField, SearchKind, SearchResult, SearchResults,
    TaskMatch, TaskSearchField,
};
pub use service::{DaemonRestartReport, RestartVia, ServicePlatform, ServiceStatus};
pub use status::StatusResponse;
pub use task::{
    AmbiguousPrefix, FocusEntry, FocusList, GoResult, IllegalTransition, PromptSegment,
    TaskActionName, TaskActionRequest, TaskCandidate, TaskDetail, TaskEvents, TaskList,
    TaskMoveRequest, TaskRank, TaskReadOnly, TaskSummary, UndoConflict,
};
pub use task_index::{FailedTaskFile, InvalidTaskFile, ReindexResult, TaskStoreHealth};
pub use update::{
    DaemonRestart, InstallReport, RollbackReport, UpgradeDecision, UpgradeReport,
    UpgradeSkipReason, UpgradeSource,
};
pub use version::{Channel, Profile, TEST_BUILD_VERSION_ENV, VersionInfo};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "loom-api");
    }
}
