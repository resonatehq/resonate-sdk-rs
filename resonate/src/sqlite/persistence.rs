//! Storage types — vendored from `resonate/src/persistence/mod.rs`.
//!
//! Trimmed to the libsql (SQLite) backend only: the parameter/result structs and
//! the `StorageError` type. The upstream `Db` trait and multi-backend `Storage`
//! enum are dropped — there is a single concrete backend ([`super::persistence_sqlite::SqliteStorage`]),
//! so its methods live as inherent `async fn`s on the concrete type.
#![allow(dead_code)]

use super::types::{PromiseRecord, TaskState};

pub type StorageResult<T> = Result<T, StorageError>;

#[derive(Debug)]
pub enum StorageError {
    /// A backend-agnostic storage error. Carries the formatted error message
    /// without exposing the underlying driver type (libsql, sqlx, etc.).
    Backend(String),
    /// Serialization conflict — retries exhausted, nothing was committed.
    /// The caller should return 503 (not 500) to indicate a retriable no-op.
    Serialization,
    /// The request contains a field that violates a storage-level constraint
    /// (e.g. a VARCHAR(255) column in MySQL). The caller should return 400.
    InvalidInput(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::Backend(msg) => write!(f, "Storage error: {}", msg),
            StorageError::Serialization => write!(f, "Serialization conflict"),
            StorageError::InvalidInput(msg) => write!(f, "Invalid input: {}", msg),
        }
    }
}

impl From<libsql::Error> for StorageError {
    fn from(e: libsql::Error) -> Self {
        StorageError::Backend(e.to_string())
    }
}

// === Result types for CTE-based operations ===

pub struct PromiseCreateResult {
    /// Whether the promise was newly inserted (false = already existed).
    pub was_created: bool,
    pub promise: PromiseRecord,
}

pub struct PromiseSettleResult {
    /// Whether the promise was actually transitioned from pending to settled.
    pub was_settled: bool,
    /// `None` when the promise was not found in the database.
    pub promise: Option<PromiseRecord>,
}

pub struct RegisterCallbackResult {
    pub awaited: Option<PromiseRecord>,
    pub awaiter: Option<PromiseRecord>,
}

pub struct TaskCreateResult {
    pub promise: PromiseRecord,
    pub task_created: bool,
    pub task_state: Option<String>,
    pub task_version: Option<i64>,
}

pub struct TaskAcquireResult {
    pub promise: Option<PromiseRecord>,
    pub was_acquired: bool,
    pub task_state: Option<TaskState>,
    pub task_version: Option<i64>,
}

pub struct TaskFenceResult {
    pub task_exists: bool,
    pub fence_ok: bool,
    pub promise: Option<PromiseRecord>,
}

pub struct TaskSuspendResult {
    pub task_matched: bool,
    pub was_suspended: bool,
    pub missing_count: i32,
}

pub struct TaskReleaseResult {
    pub task_released: bool,
    pub task_exists: bool,
}

pub struct TaskFulfillResult {
    pub task_exists: bool,
    /// Whether the task was actually transitioned to fulfilled.
    pub task_fulfilled: bool,
    /// `None` when the promise was not found in the database.
    pub promise: Option<PromiseRecord>,
}

pub struct TaskHaltResult {
    pub task_exists: bool,
    pub task_fulfilled: bool,
}

pub struct TaskContinueResult {
    pub task_exists: bool,
    pub continued: bool,
}

pub struct OutgoingExecute {
    pub id: String,
    pub version: i64,
    pub address: String,
}

pub struct OutgoingUnblock {
    pub address: String,
    pub promise: PromiseRecord,
}

// === Parameter structs for Db methods ===

pub struct PromiseCreateParams<'a> {
    pub id: &'a str,
    pub state: &'a str,
    pub param_headers: Option<&'a str>,
    pub param_data: Option<&'a str>,
    pub tags: &'a str,
    pub timeout_at: i64,
    pub created_at: i64,
    pub settled_at: Option<i64>,
    pub already_timedout: bool,
    pub address: Option<&'a str>,
}

pub struct PromiseSettleParams<'a> {
    pub id: &'a str,
    pub state: &'a str,
    pub value_headers: Option<&'a str>,
    pub value_data: Option<&'a str>,
    pub settled_at: i64,
}

pub struct TaskCreateParams<'a> {
    pub promise_id: &'a str,
    pub state: &'a str,
    pub param_headers: Option<&'a str>,
    pub param_data: Option<&'a str>,
    pub tags: &'a str,
    pub timeout_at: i64,
    pub created_at: i64,
    pub settled_at: Option<i64>,
    pub already_timedout: bool,
    pub ttl: i64,
    pub pid: &'a str,
}

pub struct TaskAcquireParams<'a> {
    pub task_id: &'a str,
    pub version: i64,
    pub time: i64,
    pub ttl: i64,
    pub pid: &'a str,
}

pub struct TaskFenceCreateParams<'a> {
    pub task_id: &'a str,
    pub version: i64,
    pub promise_id: &'a str,
    pub state: &'a str,
    pub param_headers: Option<&'a str>,
    pub param_data: Option<&'a str>,
    pub tags: &'a str,
    pub timeout_at: i64,
    pub created_at: i64,
    pub settled_at: Option<i64>,
    pub already_timedout: bool,
    pub address: Option<&'a str>,
}

pub struct TaskFenceSettleParams<'a> {
    pub task_id: &'a str,
    pub version: i64,
    pub promise_id: &'a str,
    pub state: &'a str,
    pub value_headers: Option<&'a str>,
    pub value_data: Option<&'a str>,
    pub settled_at: i64,
}

pub struct TaskFulfillParams<'a> {
    pub task_id: &'a str,
    pub version: i64,
    pub promise_id: &'a str,
    pub state: &'a str,
    pub value_headers: Option<&'a str>,
    pub value_data: Option<&'a str>,
    pub settled_at: i64,
}

pub struct ScheduleCreateParams<'a> {
    pub id: &'a str,
    pub cron: &'a str,
    pub promise_id: &'a str,
    pub promise_timeout: i64,
    pub promise_param_headers: Option<&'a str>,
    pub promise_param_data: Option<&'a str>,
    pub promise_tags: &'a str,
    pub created_at: i64,
    pub next_run_at: i64,
}
