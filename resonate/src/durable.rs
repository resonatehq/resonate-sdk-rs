use std::future::Future;

use crate::context::Context;
use crate::error::Result;
use crate::info::Info;
use crate::types::{DurableKind, Outcome};

/// The execution environment passed to a durable function.
///
/// - `Function(Info)` — leaf functions receive read-only metadata.
/// - `Workflow(Context)` — workflow functions receive a full context for sub-tasks.
#[derive(Clone, Copy)]
pub enum ExecutionEnv<'a> {
    Function(&'a Info),
    Workflow(&'a Context),
}

impl<'a> ExecutionEnv<'a> {
    /// Remote dependencies the execution is now waiting on. Leaf functions
    /// can't spawn durable work, so they never have any.
    pub(crate) async fn collect_remote_todos(&self) -> Result<Vec<String>> {
        match self {
            ExecutionEnv::Workflow(ctx) => ctx.collect_remote_todos().await,
            ExecutionEnv::Function(_) => Ok(Vec::new()),
        }
    }

    /// Resolves when this execution has signalled suspension. The driver races
    /// this against the user future; the winner determines whether we suspend.
    /// Leaf functions never suspend, so theirs stays pending forever.
    pub(crate) async fn suspended(&self) {
        match self {
            ExecutionEnv::Workflow(ctx) => ctx.suspended().await,
            ExecutionEnv::Function(_) => std::future::pending::<()>().await,
        }
    }

    /// Extract the `Context` reference, panicking if this is a `Function` env.
    pub fn into_context(self) -> &'a Context {
        match self {
            ExecutionEnv::Workflow(ctx) => ctx,
            ExecutionEnv::Function(_) => {
                panic!("expected Workflow ExecutionEnv, got Function")
            }
        }
    }

    /// Extract the `Info` reference, panicking if this is a `Workflow` env.
    pub fn into_info(self) -> &'a Info {
        match self {
            ExecutionEnv::Function(info) => info,
            ExecutionEnv::Workflow(_) => {
                panic!("expected Function ExecutionEnv, got Workflow")
            }
        }
    }
}

/// Race a user future against a suspend signal: `Some(r)` if the future
/// completed, `None` if suspension fired first (the future is dropped). `biased`
/// so a ready completion wins a simultaneous signal.
///
/// The single implementation of the race, called by every driver (core,
/// spawn-path, inline-child mini-driver, test harness). A leaf's suspend signal
/// is `pending()` forever, so it always completes.
pub(crate) async fn race_suspend<T>(
    fut: impl Future<Output = T>,
    suspend: impl Future<Output = ()>,
) -> Option<T> {
    tokio::select! {
        biased;
        r = fut => Some(r),
        _ = suspend => None,
    }
}

/// Decide whether a driven future completed or suspended, given the race result
/// (`None` ⇒ it parked) and the remote todos collected afterward. Suspends iff
/// the future parked OR left unresolved remote work; only a clean completion
/// with no outstanding todos is `Done`.
///
/// The single source of that rule, called by every driver after `race_suspend`
/// + `collect_remote_todos`. The `debug_assert` enforces that a park always
/// registers work to wait on — otherwise the task would wedge server-side
/// (suspended with nothing to resume it).
pub(crate) fn finalize_outcome<T>(result: Option<Result<T>>, remote_todos: Vec<String>) -> Outcome<T> {
    match result {
        Some(result) if remote_todos.is_empty() => Outcome::Done(result),
        _ => {
            debug_assert!(
                !remote_todos.is_empty(),
                "suspended with no remote todos — a suspend was signalled without \
                 registering any work to wait on"
            );
            Outcome::Suspended { remote_todos }
        }
    }
}

/// Trait implemented by all `#[resonate_sdk::function]`-annotated functions.
/// Provides name/kind metadata and a uniform execution interface.
///
/// Type parameters:
/// - `Args`: The function's input arguments (must be serializable).
/// - `T`: The function's return type (must be serializable).
pub trait Durable<Args, T>: Send + Sync + 'static {
    /// The registered name of this function (used for durable promise lookup).
    const NAME: &'static str;

    /// Whether this is a leaf (Function) or a workflow (Workflow).
    const KIND: DurableKind;

    /// Execute the function.
    ///
    /// The `env` parameter provides either a `Context` (for workflows) or
    /// an `Info` (for leaf functions). Pure leaf functions may ignore it.
    fn execute(&self, env: ExecutionEnv<'_>, args: Args) -> impl Future<Output = Result<T>> + Send;
}
