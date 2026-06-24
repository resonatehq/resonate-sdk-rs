//! Request handling — vendored from `resonate/src/server.rs` (the Resonate
//! server).
//!
//! The `dispatch` function and every `op_*` handler are copied verbatim. The
//! axum HTTP layer (routes, `handle_api`, SSE poll handler), the auth and
//! metrics integrations, and the multi-backend `Server` fields are dropped —
//! the SqliteNetwork drives `dispatch` directly. The only in-body changes are:
//!   * `crate::transport::is_valid_address` → `is_valid_address` (vendored)
//!   * `processing_timeouts::process_all_timeouts` → `processing::process_all_timeouts`

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;
use validator::Validate;

use super::address::is_valid_address;
use super::persistence::{
    PromiseCreateParams, PromiseSettleParams, ScheduleCreateParams, StorageError,
    TaskAcquireParams, TaskCreateParams, TaskFenceCreateParams, TaskFenceSettleParams,
    TaskFulfillParams,
};
use super::persistence_sqlite::SqliteStorage;
use super::processing;
use super::types::{
    format_validation_errors, PromiseCreateData, PromiseGetData, PromiseRegisterCallbackData,
    PromiseRegisterListenerData, PromiseResponseData, PromiseSearchData, PromiseSearchResponseData,
    PromiseSettleData, PromiseState, RequestEnvelope, ResponseEnvelope, ScheduleCreateData,
    ScheduleDeleteData, ScheduleGetData, ScheduleResponseData, ScheduleSearchData,
    ScheduleSearchResponseData, TaskAcquireData, TaskAcquireResponseData, TaskContinueData,
    TaskCreateData, TaskCreateResponseData, TaskFenceData, TaskFenceResponseData, TaskFulfillData,
    TaskFulfillResponseData, TaskGetData, TaskHaltData, TaskHeartbeatData, TaskRecord,
    TaskReleaseData, TaskResponseData, TaskSearchData, TaskSearchResponseData, TaskState,
    TaskSuspendData, TaskSuspendPreloadData,
};
use super::util;

// ============================================================================
// Configuration
// ============================================================================

/// Minimal configuration for the embedded SQLite server.
///
/// Mirrors the subset of `resonate::config::Config` fields the vendored handlers
/// and background loops reference, with the same default values.
#[derive(Debug, Clone)]
pub struct Config {
    /// Enable debug operations (`debug.*` request kinds).
    pub debug: bool,
    pub tasks: TasksConfig,
    pub timeouts: TimeoutsConfig,
    pub messages: MessagesConfig,
    pub server: ServerConfig,
}

#[derive(Debug, Clone)]
pub struct TasksConfig {
    /// Default pending task retry timeout (ms).
    pub retry_timeout: i64,
}

#[derive(Debug, Clone)]
pub struct TimeoutsConfig {
    /// Background timeout scan interval (ms).
    pub poll_interval: u64,
}

#[derive(Debug, Clone)]
pub struct MessagesConfig {
    /// Background message delivery scan interval (ms).
    pub poll_interval: u64,
    /// Max messages to claim per delivery cycle.
    pub batch_size: i64,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// External server URL included in outgoing `execute` message heads.
    pub url: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            debug: false,
            tasks: TasksConfig {
                retry_timeout: 30_000,
            },
            timeouts: TimeoutsConfig {
                poll_interval: 1_000,
            },
            messages: MessagesConfig {
                poll_interval: 100,
                batch_size: 100,
            },
            server: ServerConfig { url: None },
        }
    }
}

/// The running server — owns configuration and storage.
pub struct Server {
    pub config: Config,
    pub storage: Arc<SqliteStorage>,
    pub debug_mode: AtomicBool,
}

impl Server {
    pub fn new(config: Config, storage: SqliteStorage) -> Self {
        Self {
            config,
            storage: Arc::new(storage),
            debug_mode: AtomicBool::new(false),
        }
    }
}

pub async fn dispatch(state: &Arc<Server>, req: &RequestEnvelope, now: i64) -> ResponseEnvelope {
    let kind = req.kind.as_str();

    match kind {
        // === Promise operations ===
        "promise.get" => op_promise_get(state, req, now).await,
        "promise.create" => op_promise_create(state, req, now).await,
        "promise.settle" => op_promise_settle(state, req, now).await,
        "promise.register_callback" => op_promise_register_callback(state, req, now).await,
        "promise.register_listener" => op_promise_register_listener(state, req, now).await,
        "promise.search" => op_promise_search(state, req, now).await,

        // === Task operations ===
        "task.get" => op_task_get(state, req, now).await,
        "task.create" => op_task_create(state, req, now).await,
        "task.acquire" => op_task_acquire(state, req, now).await,
        "task.release" => op_task_release(state, req, now).await,
        "task.fulfill" => op_task_fulfill(state, req, now).await,
        "task.suspend" => op_task_suspend(state, req, now).await,
        "task.fence" => op_task_fence(state, req, now).await,
        "task.heartbeat" => op_task_heartbeat(state, req, now).await,
        "task.halt" => op_task_halt(state, req, now).await,
        "task.continue" => op_task_continue(state, req, now).await,
        "task.search" => op_task_search(state, req, now).await,

        // === Schedule operations ===
        "schedule.get" => op_schedule_get(state, req, now).await,
        "schedule.create" => op_schedule_create(state, req, now).await,
        "schedule.delete" => op_schedule_delete(state, req).await,
        "schedule.search" => op_schedule_search(state, req).await,

        // === Debug operations ===
        "debug.start" | "debug.stop" | "debug.reset" | "debug.snap" | "debug.tick"
            if !state.config.debug =>
        {
            ResponseEnvelope::error(
                req.kind.clone(),
                req.head.corr_id.clone(),
                403,
                "Debug operations are disabled",
            )
        }
        "debug.start" => {
            state.debug_mode.store(true, Ordering::SeqCst);
            tracing::info!("Debug mode started — background loops paused");
            ResponseEnvelope::new(
                req.kind.clone(),
                req.head.corr_id.clone(),
                200,
                Value::Object(serde_json::Map::new()),
            )
        }
        "debug.stop" => {
            state.debug_mode.store(false, Ordering::SeqCst);
            tracing::info!("Debug mode stopped — background loops resumed");
            ResponseEnvelope::new(
                req.kind.clone(),
                req.head.corr_id.clone(),
                200,
                Value::Object(serde_json::Map::new()),
            )
        }
        "debug.reset" => op_debug_reset(state, req).await,
        "debug.snap" => op_debug_snap(state, req).await,
        "debug.tick" => op_debug_tick(state, req).await,

        _ => {
            tracing::warn!(kind = %kind, "Invalid request: unknown operation");
            ResponseEnvelope::error(
                req.kind.clone(),
                req.head.corr_id.clone(),
                400,
                &format!("Unknown operation: {}", kind),
            )
        }
    }
}

// ============================================================================
// Promise operations
// ============================================================================

async fn op_promise_get(state: &Arc<Server>, req: &RequestEnvelope, now: i64) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: PromiseGetData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            db.try_timeout(&[&r.id], now).await?;
            match db.promise_get(&r.id).await? {
                Some(promise) => {
                    tracing::debug!(
                        promise_id = %r.id,
                        state = %promise.state,
                        "Promise found"
                    );
                    Ok(ResponseEnvelope::success(
                        kind_str.clone(),
                        corr_id.clone(),
                        &PromiseResponseData { promise },
                    ))
                }
                None => {
                    tracing::debug!(promise_id = %r.id, "Promise not found");
                    Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        404,
                        "Promise not found",
                    ))
                }
            }
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_promise_create(
    state: &Arc<Server>,
    req: &RequestEnvelope,
    now: i64,
) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: PromiseCreateData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            let address = r.tags.get("resonate:target").map(|s| s.as_str());
            if let Some(addr) = address {
                if !is_valid_address(addr) {
                    tracing::warn!(
                        promise_id = %r.id,
                        address = addr,
                        "Promise create rejected: invalid resonate:target address"
                    );
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        "Invalid resonate:target address",
                    ));
                }
            }
            db.try_timeout(&[&r.id], now).await?;
            let tags_json = serde_json::to_string(&r.tags).unwrap();
            let already_timedout = now >= r.timeout_at;
            let (state, created_at, settled_at) = if already_timedout {
                let state = if r.tags.get("resonate:timer").map(|v| v.as_str()) == Some("true") {
                    tracing::debug!(promise_id = %r.id, "Promise created already timedout (timer: resolved immediately)");
                    PromiseState::Resolved
                } else {
                    tracing::debug!(promise_id = %r.id, "Promise created already timedout");
                    PromiseState::RejectedTimedout
                };
                (state, r.timeout_at, Some(r.timeout_at))
            } else {
                (PromiseState::Pending, now, None)
            };
            let param_headers_json = r
                .param
                .headers
                .as_ref()
                .map(|h| serde_json::to_string(h).unwrap());
            let result = db.promise_create(&PromiseCreateParams {
                id: &r.id,
                state: state.as_str(),
                param_headers: param_headers_json.as_deref(),
                param_data: r.param.data.as_deref(),
                tags: &tags_json,
                timeout_at: r.timeout_at,
                created_at,
                settled_at,
                already_timedout,
                address,
            }).await?;
            if result.was_created {
                tracing::info!(
                    promise_id = %result.promise.id,
                    state = %result.promise.state,
                    timeout_at = result.promise.timeout_at,
                    target = address.unwrap_or("none"),
                    already_timedout = already_timedout,
                    "Promise created"
                );
            } else {
                tracing::debug!(
                    promise_id = %result.promise.id,
                    state = %result.promise.state,
                    "Promise create: already exists (idempotent)"
                );
            }
            Ok(ResponseEnvelope::success(
                kind_str.clone(),
                corr_id.clone(),
                &PromiseResponseData { promise: result.promise },
            ))
        }))
        .await
    {
        Ok(resp) => resp,
        Err(StorageError::InvalidInput(msg)) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            400,
            &format!("Invalid request: {}", msg),
        ),
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_promise_settle(
    state: &Arc<Server>,
    req: &RequestEnvelope,
    now: i64,
) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: PromiseSettleData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            db.try_timeout(&[&r.id], now).await?;
            let value_headers_json = r
                .value
                .headers
                .as_ref()
                .map(|h| serde_json::to_string(h).unwrap());
            let result = db.promise_settle(&PromiseSettleParams {
                id: &r.id,
                state: r.state.as_str(),
                value_headers: value_headers_json.as_deref(),
                value_data: r.value.data.as_deref(),
                settled_at: now,
            }).await?;
            match result.promise {
                Some(promise) => {
                    assert_ne!(
                        promise.state,
                        PromiseState::Pending,
                        "invariant: returning 200 but promise is still pending"
                    );
                    if result.was_settled {
                        tracing::info!(
                            promise_id = %promise.id,
                            state = %promise.state,
                            "Promise settled"
                        );
                    } else {
                        tracing::debug!(
                            promise_id = %promise.id,
                            current_state = %promise.state,
                            requested_state = %r.state,
                            "Promise settle: already settled (idempotent)"
                        );
                    }
                    Ok(ResponseEnvelope::success(
                        kind_str.clone(),
                        corr_id.clone(),
                        &PromiseResponseData { promise },
                    ))
                }
                None => {
                    tracing::debug!(promise_id = %r.id, "Promise settle: promise not found");
                    Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        404,
                        "Promise not found",
                    ))
                }
            }
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_promise_register_callback(
    state: &Arc<Server>,
    req: &RequestEnvelope,
    now: i64,
) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: PromiseRegisterCallbackData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            db.try_timeout(&[&r.awaited, &r.awaiter], now).await?;
            let result = db.promise_register_callback(&r.awaited, &r.awaiter, now).await?;
            let p_awaited = match result.awaited {
                Some(p) => p,
                None => {
                    tracing::debug!(promise_id = %r.awaited, "Callback registration: awaited promise not found");
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        404,
                        "Awaited promise not found",
                    ))
                }
            };
            let p_awaiter = match result.awaiter {
                Some(p) => p,
                None => {
                    tracing::debug!(promise_id = %r.awaiter, "Callback registration: awaiter promise not found");
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        422,
                        "Awaiter promise not found",
                    ))
                }
            };
            if !p_awaiter.tags.contains_key("resonate:target") {
                tracing::debug!(awaiter = %r.awaiter, "Callback registration rejected: awaiter has no resonate:target");
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    422,
                    "Awaiter promise has no resonate:target tag",
                ));
            }
            tracing::info!(
                awaited = %r.awaited,
                awaiter = %r.awaiter,
                awaited_state = %p_awaited.state,
                "Callback registered"
            );
            Ok(ResponseEnvelope::success(
                kind_str.clone(),
                corr_id.clone(),
                &PromiseResponseData { promise: p_awaited },
            ))
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_promise_register_listener(
    state: &Arc<Server>,
    req: &RequestEnvelope,
    now: i64,
) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: PromiseRegisterListenerData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            if !is_valid_address(&r.address) {
                tracing::warn!(
                    awaited = %r.awaited,
                    address = %r.address,
                    "Listener registration rejected: invalid address"
                );
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    "Invalid listener address",
                ));
            }
            db.try_timeout(&[&r.awaited], now).await?;
            match db.promise_register_listener(&r.awaited, &r.address).await? {
                Some(promise) => {
                    tracing::info!(
                        awaited = %r.awaited,
                        address = %r.address,
                        promise_state = %promise.state,
                        "Listener registered"
                    );
                    Ok(ResponseEnvelope::success(
                        kind_str.clone(),
                        corr_id.clone(),
                        &PromiseResponseData { promise },
                    ))
                }
                None => {
                    tracing::debug!(awaited = %r.awaited, "Listener registration: awaited promise not found");
                    Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        404,
                        "Awaited promise not found",
                    ))
                }
            }
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_promise_search(
    state: &Arc<Server>,
    req: &RequestEnvelope,
    _now: i64,
) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: PromiseSearchData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            let tags_json = r.tags.as_ref().map(|t| serde_json::to_string(t).unwrap());
            let limit = match r.limit {
                Some(n) if n > 1000 => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        "Invalid 'limit' — must be between 1 and 1000",
                    ))
                }
                Some(n) => n,
                None => 100,
            };
            let state_str = r.state.map(|s| s.as_str());
            let results = db.promise_search(
                state_str,
                tags_json.as_deref(),
                r.cursor.as_deref(),
                limit + 1,
            ).await?;
            let has_more = results.len() as i64 > limit;
            let promises: Vec<_> = results.into_iter().take(limit as usize).collect();
            let next_cursor = if has_more {
                promises.last().map(|p| p.id.clone())
            } else {
                None
            };
            tracing::debug!(
                found = promises.len(),
                has_more = has_more,
                "Promise search completed"
            );
            Ok(ResponseEnvelope::success(
                kind_str.clone(),
                corr_id.clone(),
                &PromiseSearchResponseData {
                    promises,
                    cursor: next_cursor,
                },
            ))
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

// ============================================================================
// Task operations
// ============================================================================

async fn op_task_get(state: &Arc<Server>, req: &RequestEnvelope, now: i64) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: TaskGetData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            db.try_timeout(&[&r.id], now).await?;
            match db.task_get(&r.id).await? {
                Some(task) => {
                    tracing::debug!(
                        task_id = %r.id,
                        state = %task.state,
                        version = task.version,
                        "Task found"
                    );
                    Ok(ResponseEnvelope::success(
                        kind_str.clone(),
                        corr_id.clone(),
                        &TaskResponseData { task },
                    ))
                }
                None => {
                    tracing::debug!(task_id = %r.id, "Task not found");
                    Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        404,
                        "Task not found",
                    ))
                }
            }
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_task_create(state: &Arc<Server>, req: &RequestEnvelope, now: i64) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: TaskCreateData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            let action_data = &r.action.data;
            let action_id = &action_data.id;
            if let Some(addr) = action_data.tags.get("resonate:target") {
                if !is_valid_address(addr) {
                    tracing::warn!(
                        task_id = %action_id,
                        address = %addr,
                        "Task create rejected: invalid resonate:target address"
                    );
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        "Invalid resonate:target address",
                    ));
                }
            }
            db.try_timeout(&[action_id], now).await?;
            // Lock preamble: ensures CTE and subsequent reads see
            // current state under READ COMMITTED.
            let _ = db.lock_for_update(action_id).await?;
            let tags_json = serde_json::to_string(&action_data.tags).unwrap();
            let already_timedout = now >= action_data.timeout_at;
            let (p_state, created_at, settled_at) = if already_timedout {
                let p_state =
                    if action_data.tags.get("resonate:timer").map(|v| v.as_str()) == Some("true") {
                        tracing::debug!(task_id = %action_id, "Task create: already timedout (timer: resolved immediately)");
                        PromiseState::Resolved
                    } else {
                        tracing::debug!(task_id = %action_id, "Task create: already timedout");
                        PromiseState::RejectedTimedout
                    };
                (
                    p_state,
                    action_data.timeout_at,
                    Some(action_data.timeout_at),
                )
            } else {
                (PromiseState::Pending, now, None)
            };
            let param_headers_json = action_data
                .param
                .headers
                .as_ref()
                .map(|h| serde_json::to_string(h).unwrap());
            let res = db.task_create(&TaskCreateParams {
                promise_id: action_id,
                state: p_state.as_str(),
                param_headers: param_headers_json.as_deref(),
                param_data: action_data.param.data.as_deref(),
                tags: &tags_json,
                timeout_at: action_data.timeout_at,
                created_at,
                settled_at,
                already_timedout,
                ttl: r.ttl,
                pid: &r.pid,
            }).await?;

            // If the promise is settled, process callbacks as a separate
            // statement. This fires any callbacks registered by concurrent
            // transactions (e.g. task.suspend) that committed after
            // try_timeout's snapshot but before now.
            if res.promise.state != PromiseState::Pending {
                db.process_callbacks(action_id, now).await?;
            }

            // When the CTE created the task, use CTE result directly.
            if res.task_created {
                let task_state_str = res.task_state.expect("invariant: task_state is Some when task_created");
                let task_state = task_state_str.parse::<TaskState>().expect("invariant: task_state is a valid TaskState");
                assert!(res.promise.state != PromiseState::Pending || task_state != TaskState::Fulfilled, "invariant: pending promise with fulfilled task");
                assert!(res.promise.state == PromiseState::Pending || task_state == TaskState::Fulfilled, "invariant: settled promise with non-fulfilled task");
                // Acquired tasks start at version 1 (first claim), fulfilled at 0
                let task_version = if task_state == TaskState::Acquired { 1 } else { 0 };
                let task = TaskRecord {
                    id: action_id.to_string(),
                    state: task_state,
                    version: task_version,
                    resumes: 0,
                    ttl: if task_state == TaskState::Fulfilled { None } else { Some(r.ttl) },
                    pid: if task_state == TaskState::Fulfilled { None } else { Some(r.pid.to_string()) },
                };
                let preload = if task_state == TaskState::Acquired {
                    db.compute_preload(action_id).await?
                } else {
                    vec![]
                };
                return Ok(ResponseEnvelope::success(
                    kind_str.clone(),
                    corr_id.clone(),
                    &TaskCreateResponseData {
                        task,
                        promise: res.promise,
                        preload,
                    },
                ));
            }

            // CTE didn't create the task (promise already existed).
            // Branch on the state/version surfaced by the CTE.
            match (res.task_state.as_deref(), res.task_version) {
                (Some("fulfilled"), version) => {
                    assert_ne!(res.promise.state, PromiseState::Pending, "invariant: pending promise with fulfilled task");
                    Ok(ResponseEnvelope::success(
                        kind_str.clone(),
                        corr_id.clone(),
                        &TaskCreateResponseData {
                            task: TaskRecord {
                                id: action_id.to_string(),
                                state: TaskState::Fulfilled,
                                version: version.unwrap_or(0),
                                resumes: 0,
                                ttl: None,
                                pid: None,
                            },
                            promise: res.promise,
                            preload: vec![],
                        },
                    ))
                }
                (Some("pending"), Some(version)) => {
                    let acquire_result = db.task_acquire(&TaskAcquireParams {
                        task_id: action_id,
                        version,
                        time: now,
                        ttl: r.ttl,
                        pid: &r.pid,
                    }).await?;
                    if acquire_result.was_acquired {
                        let task = TaskRecord {
                            id: action_id.to_string(),
                            state: TaskState::Acquired,
                            version: version + 1,
                            resumes: 0,
                            ttl: Some(r.ttl),
                            pid: Some(r.pid.to_string()),
                        };
                        assert_eq!(res.promise.state, PromiseState::Pending, "invariant: settled promise with non-fulfilled task");
                        assert_eq!(acquire_result.task_version, Some(version + 1), "invariant: acquired task version must be version + 1");
                        let preload = db.compute_preload(action_id).await?;
                        Ok(ResponseEnvelope::success(
                            kind_str.clone(),
                            corr_id.clone(),
                            &TaskCreateResponseData {
                                task,
                                promise: res.promise,
                                preload,
                            },
                        ))
                    } else if acquire_result.task_state == Some(TaskState::Fulfilled) {
                        let promise = acquire_result.promise.expect("fulfilled task must have a promise");
                        assert_ne!(promise.state, PromiseState::Pending, "invariant: fulfilled task cannot have a pending promise");
                        Ok(ResponseEnvelope::success(
                            kind_str.clone(),
                            corr_id.clone(),
                            &TaskCreateResponseData {
                                task: TaskRecord {
                                    id: action_id.to_string(),
                                    state: TaskState::Fulfilled,
                                    version: acquire_result.task_version.expect("invariant: fulfilled task must have a version"),
                                    resumes: 0,
                                    ttl: None,
                                    pid: None,
                                },
                                promise,
                                preload: vec![],
                            },
                        ))
                    } else {
                        assert!(acquire_result.task_state.is_some(), "invariant: non-acquired result must have a task state");
                        assert!(acquire_result.task_version.is_some(), "invariant: non-acquired result must have a task version");
                        assert!(
                            acquire_result.task_state.unwrap() != TaskState::Pending || acquire_result.task_version.unwrap() != version,
                            "invariant: task state must not be pending or version must differ from request"
                        );
                        Ok(ResponseEnvelope::error(
                            kind_str.clone(),
                            corr_id.clone(),
                            409,
                            "Already exists",
                        ))
                    }
                }
                (None, _) => Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    422,
                    "Promise exists without a target task",
                )),
                _ => Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    409,
                    "Already exists",
                )),
            }
        }))
        .await
    {
        Ok(resp) => resp,
        Err(StorageError::InvalidInput(msg)) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            400,
            &format!("Invalid request: {}", msg),
        ),
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_task_acquire(state: &Arc<Server>, req: &RequestEnvelope, now: i64) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: TaskAcquireData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            db.try_timeout(&[&r.id], now).await?;
            let result = db.task_acquire(&TaskAcquireParams {
                task_id: &r.id,
                version: r.version,
                time: now,
                ttl: r.ttl,
                pid: &r.pid,
            }).await?;
            match result.promise {
                None => {
                    tracing::debug!(task_id = %r.id, "Task acquire: task not found");
                    Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        404,
                        "Task not found",
                    ))
                }
                Some(promise) => {
                    assert!(result.task_state.is_some(), "invariant: acquired result must have a task state");
                    assert!(result.task_version.is_some(), "invariant: acquired result must have a task version");
                    assert!(
                        result.task_state.unwrap() != TaskState::Pending || result.task_version.unwrap() != r.version,
                        "invariant: task state must not be pending or version must differ from request"
                    );
                    if !result.was_acquired {
                        let state = result.task_state.unwrap();
                        let version = result.task_version.unwrap();
                        if state != TaskState::Pending {
                            tracing::debug!(
                                task_id = %r.id,
                                current_state = %state,
                                "Task acquire rejected: not pending"
                            );
                            return Ok(ResponseEnvelope::error(
                                kind_str.clone(),
                                corr_id.clone(),
                                409,
                                "Task is not pending",
                            ));
                        }
                        tracing::debug!(
                            task_id = %r.id,
                            expected_version = r.version,
                            actual_version = version,
                            "Task acquire rejected: version mismatch"
                        );
                        return Ok(ResponseEnvelope::error(
                            kind_str.clone(),
                            corr_id.clone(),
                            409,
                            "Version mismatch",
                        ));
                    }
                    assert_eq!(result.task_version, Some(r.version + 1), "invariant: acquired task version must be request version + 1");
                    // Use known values — no separate task_get that could
                    // see stale state from concurrent transactions.
                    let task = TaskRecord {
                        id: r.id.to_string(),
                        state: TaskState::Acquired,
                        version: r.version + 1,
                        resumes: 0,
                        ttl: Some(r.ttl),
                        pid: Some(r.pid.to_string()),
                    };
                    let preload = db.compute_preload(&r.id).await?;
                    Ok(ResponseEnvelope::success(
                        kind_str.clone(),
                        corr_id.clone(),
                        &TaskAcquireResponseData {
                            task,
                            promise,
                            preload,
                        },
                    ))
                }
            }
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_task_release(state: &Arc<Server>, req: &RequestEnvelope, now: i64) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: TaskReleaseData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            db.try_timeout(&[&r.id], now).await?;
            let (_, task_exists) = db.lock_for_update(&r.id).await?;
            if !task_exists {
                tracing::debug!(task_id = %r.id, "Task release: task not found");
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    404,
                    "Task not found",
                ));
            }
            let result = db.task_release(&r.id, r.version, now, db.task_retry_timeout()).await?;
            if result.task_released {
                tracing::info!(task_id = %r.id, version = r.version, "Task released back to pending");
                return Ok(ResponseEnvelope::new(
                    kind_str.clone(),
                    corr_id.clone(),
                    200,
                    serde_json::json!({}),
                ));
            }
            if !result.task_exists {
                tracing::debug!(task_id = %r.id, "Task release: task not found");
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    404,
                    "Task not found",
                ));
            }
            tracing::debug!(task_id = %r.id, version = r.version, "Task release rejected: version mismatch or invalid state");
            Ok(ResponseEnvelope::error(
                kind_str.clone(),
                corr_id.clone(),
                409,
                "Task version mismatch or invalid state",
            ))
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_task_fulfill(state: &Arc<Server>, req: &RequestEnvelope, now: i64) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: TaskFulfillData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            let action_data = &r.action.data;
            db.try_timeout(&[&action_data.id], now).await?;
            // Lock preamble: lock promise + task to prevent stale snapshot
            // in fulfillment CTE.
            let (_, task_exists) = db.lock_for_update(&r.id).await?;
            if !task_exists {
                tracing::debug!(task_id = %r.id, "Task fulfill: task not found");
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    404,
                    "Task not found",
                ));
            }
            let value_headers_json = action_data
                .value
                .headers
                .as_ref()
                .map(|h| serde_json::to_string(h).unwrap());
            let result = db.task_fulfill(&TaskFulfillParams {
                task_id: &r.id,
                version: r.version,
                promise_id: &r.id,
                state: action_data.state.as_str(),
                value_headers: value_headers_json.as_deref(),
                value_data: action_data.value.data.as_deref(),
                settled_at: now,
            }).await?;
            if !result.task_exists {
                tracing::debug!(task_id = %r.id, "Task fulfill: task not found");
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    404,
                    "Task not found",
                ));
            }
            if !result.task_fulfilled {
                tracing::debug!(task_id = %r.id, version = r.version, "Task fulfill rejected: version mismatch or invalid state");
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    409,
                    "Task version mismatch or invalid state",
                ));
            }
            let promise = result.promise.expect("invariant: task exists implies promise exists");
            assert!(result.task_fulfilled, "invariant: returning 200 but task is not fulfilled");
            assert_ne!(promise.state, PromiseState::Pending, "invariant: returning 200 but promise is still pending");
            tracing::info!(
                task_id = %r.id,
                version = r.version,
                promise_state = %promise.state,
                "Task fulfilled and promise settled"
            );
            Ok(ResponseEnvelope::success(
                kind_str.clone(),
                corr_id.clone(),
                &TaskFulfillResponseData { promise },
            ))
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_task_suspend(state: &Arc<Server>, req: &RequestEnvelope, now: i64) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: TaskSuspendData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            let awaited_ids: Vec<String> =
                r.actions.iter().map(|a| a.data.awaited.clone()).collect();
            let mut timeout_ids: Vec<&str> = vec![&r.id];
            for aid in &awaited_ids {
                timeout_ids.push(aid.as_str());
            }
            // Lock the task row BEFORE try_timeout to prevent
            // try_timeout from fulfilling it via promise timeout.
            let (_, task_exists) = db.lock_for_update(&r.id).await?;
            db.try_timeout(&timeout_ids, now).await?;
            let mut seen = std::collections::HashSet::new();
            let unique_awaited: Vec<&str> = awaited_ids
                .iter()
                .filter(|id| seen.insert(id.as_str()))
                .map(|s| s.as_str())
                .collect();
            let result = db.task_suspend(&r.id, r.version, &unique_awaited).await?;
            if !result.task_matched {
                // Use lock_for_update result — no separate task_get that
                // could see a concurrent task creation.
                if !task_exists {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        404,
                        "Task not found",
                    ));
                }
                tracing::debug!(
                    task_id = %r.id,
                    version = r.version,
                    "Task suspend rejected: not acquired or version mismatch"
                );
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    409,
                    "Task is not acquired or version mismatch",
                ));
            }
            if result.missing_count > 0 {
                tracing::debug!(
                    task_id = %r.id,
                    missing_count = result.missing_count,
                    "Task suspend rejected: awaited promise(s) not found"
                );
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    422,
                    "Awaited promise not found",
                ));
            }
            if result.was_suspended {
                tracing::info!(
                    task_id = %r.id,
                    version = r.version,
                    awaited_count = unique_awaited.len(),
                    "Task suspended, waiting on promises"
                );
                return Ok(ResponseEnvelope::new(
                    kind_str.clone(),
                    corr_id.clone(),
                    200,
                    serde_json::json!({}),
                ));
            }
            tracing::info!(
                task_id = %r.id,
                version = r.version,
                "Task suspend: immediate resume, awaited promises already settled"
            );
            let preload = db.compute_preload(&r.id).await?;
            Ok(ResponseEnvelope::new(
                kind_str.clone(),
                corr_id.clone(),
                300,
                serde_json::to_value(&TaskSuspendPreloadData { preload }).unwrap(),
            ))
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_task_fence(state: &Arc<Server>, req: &RequestEnvelope, now: i64) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: TaskFenceData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            let action_kind = &r.action.kind;
            let action_data = &r.action.data;
            let action_id = action_data["id"].as_str().unwrap_or("");
            db.try_timeout(&[&r.id, action_id], now).await?;
            // Lock preamble: ensures fence check sees current task state.
            let _ = db.lock_for_update(&r.id).await?;

            match action_kind.as_str() {
                "promise.create" => {
                    let create_data: PromiseCreateData =
                        match serde_json::from_value(action_data.clone()) {
                            Ok(d) => d,
                            Err(e) => {
                                return Ok(ResponseEnvelope::error(
                                    kind_str.clone(),
                                    corr_id.clone(),
                                    400,
                                    &format!("Invalid action data: {}", e),
                                ))
                            }
                        };
                    if let Err(e) = create_data.validate() {
                        return Ok(ResponseEnvelope::error(
                            kind_str.clone(),
                            corr_id.clone(),
                            400,
                            &format_validation_errors(&e),
                        ));
                    }
                    let tags_json = serde_json::to_string(&create_data.tags).unwrap();
                    let already_timedout = now >= create_data.timeout_at;
                    let address = create_data.tags.get("resonate:target").map(|s| s.as_str());
                    if let Some(addr) = address {
                        if !is_valid_address(addr) {
                            tracing::warn!(
                                task_id = %r.id,
                                address = addr,
                                "Task fence rejected: invalid resonate:target address in fenced promise.create"
                            );
                            return Ok(ResponseEnvelope::error(
                                kind_str.clone(),
                                corr_id.clone(),
                                400,
                                "Invalid resonate:target address",
                            ));
                        }
                    }
                    let (p_state, created_at, settled_at) = if already_timedout {
                        let p_state = if create_data.tags.get("resonate:timer").map(|v| v.as_str())
                            == Some("true")
                        {
                            PromiseState::Resolved
                        } else {
                            PromiseState::RejectedTimedout
                        };
                        (
                            p_state,
                            create_data.timeout_at,
                            Some(create_data.timeout_at),
                        )
                    } else {
                        (PromiseState::Pending, now, None)
                    };
                    let param_headers_json = create_data
                        .param
                        .headers
                        .as_ref()
                        .map(|h| serde_json::to_string(h).unwrap());
                    let result = db.task_fence_create(&TaskFenceCreateParams {
                        task_id: &r.id,
                        version: r.version,
                        promise_id: &create_data.id,
                        state: p_state.as_str(),
                        param_headers: param_headers_json.as_deref(),
                        param_data: create_data.param.data.as_deref(),
                        tags: &tags_json,
                        timeout_at: create_data.timeout_at,
                        created_at,
                        settled_at,
                        already_timedout,
                        address,
                    }).await?;
                    if !result.task_exists {
                        tracing::debug!(task_id = %r.id, fenced_action = "promise.create", "Task fence rejected: task not found");
                        return Ok(ResponseEnvelope::error(
                            kind_str.clone(),
                            corr_id.clone(),
                            404,
                            "Task not found",
                        ));
                    }
                    if !result.fence_ok {
                        tracing::debug!(task_id = %r.id, version = r.version, fenced_action = "promise.create", "Task fence rejected: version mismatch");
                        return Ok(ResponseEnvelope::error(
                            kind_str.clone(),
                            corr_id.clone(),
                            409,
                            "Version mismatch",
                        ));
                    }
                    tracing::info!(
                        task_id = %r.id,
                        version = r.version,
                        fenced_action = "promise.create",
                        promise_id = %create_data.id,
                        "Task fence: promise.create executed"
                    );
                    let p = result.promise.expect("invariant: promise.create result must have a promise");
                    let inner_data = serde_json::json!({ "promise": p });
                    let inner_envelope = serde_json::json!({
                        "kind": action_kind,
                        "head": { "corrId": corr_id, "status": 200, "version": "2026-04-01" },
                        "data": inner_data,
                    });
                    let preload = db.compute_preload(&r.id).await?;
                    Ok(ResponseEnvelope::success(
                        kind_str.clone(),
                        corr_id.clone(),
                        &TaskFenceResponseData {
                            action: inner_envelope,
                            preload,
                        },
                    ))
                }
                "promise.settle" => {
                    let settle_data: PromiseSettleData =
                        match serde_json::from_value(action_data.clone()) {
                            Ok(d) => d,
                            Err(e) => {
                                return Ok(ResponseEnvelope::error(
                                    kind_str.clone(),
                                    corr_id.clone(),
                                    400,
                                    &format!("Invalid action data: {}", e),
                                ))
                            }
                        };
                    if let Err(e) = settle_data.validate() {
                        return Ok(ResponseEnvelope::error(
                            kind_str.clone(),
                            corr_id.clone(),
                            400,
                            &format_validation_errors(&e),
                        ));
                    }
                    let value_headers_json = settle_data
                        .value
                        .headers
                        .as_ref()
                        .map(|h| serde_json::to_string(h).unwrap());
                    let result = db.task_fence_settle(&TaskFenceSettleParams {
                        task_id: &r.id,
                        version: r.version,
                        promise_id: &settle_data.id,
                        state: settle_data.state.as_str(),
                        value_headers: value_headers_json.as_deref(),
                        value_data: settle_data.value.data.as_deref(),
                        settled_at: now,
                    }).await?;
                    if !result.task_exists {
                        tracing::debug!(task_id = %r.id, fenced_action = "promise.settle", "Task fence rejected: task not found");
                        return Ok(ResponseEnvelope::error(
                            kind_str.clone(),
                            corr_id.clone(),
                            404,
                            "Task not found",
                        ));
                    }
                    if !result.fence_ok {
                        tracing::debug!(task_id = %r.id, version = r.version, fenced_action = "promise.settle", "Task fence rejected: version mismatch");
                        return Ok(ResponseEnvelope::error(
                            kind_str.clone(),
                            corr_id.clone(),
                            409,
                            "Version mismatch",
                        ));
                    }
                    tracing::info!(
                        task_id = %r.id,
                        version = r.version,
                        fenced_action = "promise.settle",
                        promise_id = %settle_data.id,
                        settle_state = %settle_data.state,
                        "Task fence: promise.settle executed"
                    );
                    let inner_status = if result.promise.is_some() { 200 } else { 404 };
                    let inner_data = match &result.promise {
                        Some(p) => {
                            assert_ne!(p.state, PromiseState::Pending, "invariant: returning 200 but promise is still pending");
                            serde_json::json!({ "promise": p })
                        }
                        None => serde_json::json!("Promise not found"),
                    };
                    let inner_envelope = serde_json::json!({
                        "kind": action_kind,
                        "head": { "corrId": corr_id, "status": inner_status, "version": "2026-04-01" },
                        "data": inner_data,
                    });
                    let preload = db.compute_preload(&r.id).await?;
                    Ok(ResponseEnvelope::success(
                        kind_str.clone(),
                        corr_id.clone(),
                        &TaskFenceResponseData {
                            action: inner_envelope,
                            preload,
                        },
                    ))
                }
                _ => {
                    tracing::warn!(
                        task_id = %r.id,
                        action_kind = %action_kind,
                        "Task fence rejected: invalid fence action kind"
                    );
                    Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        "Invalid fence action kind",
                    ))
                }
            }
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_task_heartbeat(
    state: &Arc<Server>,
    req: &RequestEnvelope,
    now: i64,
) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: TaskHeartbeatData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            let task_pairs: Vec<(&str, i64)> =
                r.tasks.iter().map(|t| (t.id.as_str(), t.version)).collect();
            db.task_heartbeat(&r.pid, &task_pairs, now).await?;
            tracing::debug!(
                pid = %r.pid,
                task_count = task_pairs.len(),
                "Task heartbeat processed"
            );
            Ok(ResponseEnvelope::new(
                kind_str.clone(),
                corr_id.clone(),
                200,
                serde_json::json!({}),
            ))
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_task_halt(state: &Arc<Server>, req: &RequestEnvelope, now: i64) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: TaskHaltData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            db.try_timeout(&[&r.id], now).await?;
            let result = db.task_halt(&r.id).await?;
            if !result.task_exists {
                tracing::debug!(task_id = %r.id, "Task halt: not found");
                Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    404,
                    "Task not found",
                ))
            } else if result.task_fulfilled {
                tracing::debug!(task_id = %r.id, "Task halt rejected: already fulfilled");
                Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    409,
                    "Task is fulfilled",
                ))
            } else {
                tracing::info!(task_id = %r.id, "Task halted");
                Ok(ResponseEnvelope::new(
                    kind_str.clone(),
                    corr_id.clone(),
                    200,
                    serde_json::json!({}),
                ))
            }
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_task_continue(
    state: &Arc<Server>,
    req: &RequestEnvelope,
    now: i64,
) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: TaskContinueData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            db.try_timeout(&[&r.id], now).await?;
            let result = db.task_continue(&r.id, now).await?;
            if !result.task_exists {
                tracing::debug!(task_id = %r.id, "Task continue: not found");
                Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    404,
                    "Task not found",
                ))
            } else if result.continued {
                tracing::info!(task_id = %r.id, "Task continued from halted state");
                Ok(ResponseEnvelope::new(
                    kind_str.clone(),
                    corr_id.clone(),
                    200,
                    serde_json::json!({}),
                ))
            } else {
                tracing::debug!(task_id = %r.id, "Task continue rejected: not halted");
                Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    409,
                    "Task is not halted",
                ))
            }
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_task_search(state: &Arc<Server>, req: &RequestEnvelope, _now: i64) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: TaskSearchData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            let limit = match r.limit {
                Some(n) if n > 1000 => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        "Invalid 'limit' — must be between 1 and 1000",
                    ))
                }
                Some(n) => n,
                None => 100,
            };
            let state_str = r.state.map(|s| s.as_str());
            let results = db.task_search(state_str, r.cursor.as_deref(), limit + 1).await?;
            let has_more = results.len() as i64 > limit;
            let tasks: Vec<_> = results.into_iter().take(limit as usize).collect();
            let next_cursor = if has_more {
                tasks.last().map(|t| t.id.clone())
            } else {
                None
            };
            tracing::debug!(
                found = tasks.len(),
                has_more = has_more,
                "Task search completed"
            );
            Ok(ResponseEnvelope::success(
                kind_str.clone(),
                corr_id.clone(),
                &TaskSearchResponseData {
                    tasks,
                    cursor: next_cursor,
                },
            ))
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

// ============================================================================
// Schedule operations
// ============================================================================

async fn op_schedule_get(
    state: &Arc<Server>,
    req: &RequestEnvelope,
    _now: i64,
) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: ScheduleGetData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            match db.schedule_get(&r.id).await? {
                Some(schedule) => {
                    tracing::debug!(
                        schedule_id = %r.id,
                        cron = %schedule.cron,
                        next_run_at = schedule.next_run_at,
                        "Schedule found"
                    );
                    Ok(ResponseEnvelope::success(
                        kind_str.clone(),
                        corr_id.clone(),
                        &ScheduleResponseData { schedule },
                    ))
                }
                None => {
                    tracing::debug!(schedule_id = %r.id, "Schedule not found");
                    Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        404,
                        "Schedule not found",
                    ))
                }
            }
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_schedule_create(
    state: &Arc<Server>,
    req: &RequestEnvelope,
    now: i64,
) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: ScheduleCreateData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            if !util::is_valid_cron(&r.cron) {
                tracing::warn!(
                    schedule_id = %r.id,
                    cron = %r.cron,
                    "Schedule create rejected: invalid cron expression"
                );
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    "Invalid cron expression",
                ));
            }
            let promise_tags_json = serde_json::to_string(&r.promise_tags).unwrap();
            let next_run_at = util::compute_next_cron(&r.cron, now);
            let promise_param_headers_json = r
                .promise_param
                .headers
                .as_ref()
                .map(|h| serde_json::to_string(h).unwrap());
            let schedule = db.schedule_create(&ScheduleCreateParams {
                id: &r.id,
                cron: &r.cron,
                promise_id: &r.promise_id,
                promise_timeout: r.promise_timeout,
                promise_param_headers: promise_param_headers_json.as_deref(),
                promise_param_data: r.promise_param.data.as_deref(),
                promise_tags: &promise_tags_json,
                created_at: now,
                next_run_at,
            }).await?;
            tracing::info!(
                schedule_id = %schedule.id,
                cron = %schedule.cron,
                next_run_at = schedule.next_run_at,
                "Schedule created"
            );
            Ok(ResponseEnvelope::success(
                kind_str.clone(),
                corr_id.clone(),
                &ScheduleResponseData { schedule },
            ))
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_schedule_delete(state: &Arc<Server>, req: &RequestEnvelope) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: ScheduleDeleteData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            if db.schedule_delete(&r.id).await? {
                tracing::info!(schedule_id = %r.id, "Schedule deleted");
                Ok(ResponseEnvelope::new(
                    kind_str.clone(),
                    corr_id.clone(),
                    200,
                    serde_json::json!({}),
                ))
            } else {
                tracing::debug!(schedule_id = %r.id, "Schedule delete: not found");
                Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    404,
                    "Schedule not found",
                ))
            }
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

async fn op_schedule_search(state: &Arc<Server>, req: &RequestEnvelope) -> ResponseEnvelope {
    let data = req.data.clone();
    let kind_str = req.kind.clone();
    let corr_id = req.head.corr_id.clone();
    match state
        .storage
        .transact(move |db| Box::pin(async move {
            let r: ScheduleSearchData = match serde_json::from_value(data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        &format!("Invalid request: {}", e),
                    ))
                }
            };
            if let Err(e) = r.validate() {
                return Ok(ResponseEnvelope::error(
                    kind_str.clone(),
                    corr_id.clone(),
                    400,
                    &format_validation_errors(&e),
                ));
            }
            let tags_json = r.tags.as_ref().map(|t| serde_json::to_string(t).unwrap());
            let limit = match r.limit {
                Some(n) if n > 1000 => {
                    return Ok(ResponseEnvelope::error(
                        kind_str.clone(),
                        corr_id.clone(),
                        400,
                        "Invalid 'limit' — must be between 1 and 1000",
                    ))
                }
                Some(n) => n,
                None => 10,
            };
            let schedules =
                db.schedule_search(tags_json.as_deref(), r.cursor.as_deref(), limit + 1).await?;
            let limit_usize = limit as usize;
            let has_more = schedules.len() > limit_usize;
            let result_schedules: Vec<_> = schedules.into_iter().take(limit_usize).collect();
            let next_cursor = if has_more {
                result_schedules.last().map(|s| s.id.clone())
            } else {
                None
            };
            tracing::debug!(
                found = result_schedules.len(),
                has_more = has_more,
                "Schedule search completed"
            );
            Ok(ResponseEnvelope::success(
                kind_str.clone(),
                corr_id.clone(),
                &ScheduleSearchResponseData {
                    schedules: result_schedules,
                    cursor: next_cursor,
                },
            ))
        }))
        .await
    {
        Ok(resp) => resp,
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Internal error: {}", e),
        ),
    }
}

// ============================================================================
// Debug operations
// ============================================================================

async fn op_debug_reset(state: &Arc<Server>, req: &RequestEnvelope) -> ResponseEnvelope {
    match state.storage.transact(move |db| Box::pin(async move { db.debug_reset().await })).await {
        Ok(()) => {
            tracing::warn!("Debug reset: all data cleared");
            ResponseEnvelope::new(
                req.kind.clone(),
                req.head.corr_id.clone(),
                200,
                Value::Object(serde_json::Map::new()),
            )
        }
        Err(e) => {
            tracing::error!(error = %e, "Debug reset failed");
            ResponseEnvelope::error(
                req.kind.clone(),
                req.head.corr_id.clone(),
                500,
                &format!("Reset failed: {}", e),
            )
        }
    }
}

async fn op_debug_snap(state: &Arc<Server>, req: &RequestEnvelope) -> ResponseEnvelope {
    match state.storage.query(move |db| Box::pin(async move { db.snap().await })).await {
        Ok(snapshot) => {
            let data = serde_json::to_value(snapshot).unwrap_or(Value::Null);
            ResponseEnvelope::new(req.kind.clone(), req.head.corr_id.clone(), 200, data)
        }
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Snap failed: {}", e),
        ),
    }
}

async fn op_debug_tick(state: &Arc<Server>, req: &RequestEnvelope) -> ResponseEnvelope {
    let time = match req.data.get("time").and_then(|v| v.as_i64()) {
        Some(t) => t,
        None => {
            return ResponseEnvelope::error(
                req.kind.clone(),
                req.head.corr_id.clone(),
                400,
                "Missing or invalid 'time' field",
            )
        }
    };

    match state
        .storage
        .transact(move |db| Box::pin(async move { processing::process_all_timeouts(db, time).await }))
        .await
    {
        Ok(_) => ResponseEnvelope::new(
            req.kind.clone(),
            req.head.corr_id.clone(),
            200,
            Value::Array(vec![]),
        ),
        Err(e) => ResponseEnvelope::error(
            req.kind.clone(),
            req.head.corr_id.clone(),
            500,
            &format!("Tick failed: {}", e),
        ),
    }
}
