//! End-to-end integration tests.
//!
//! By default these run against the embedded, in-process [`SqliteServer`]
//! (no external server required). When the `RESONATE_URL` environment variable
//! is set, they run against that real Resonate server instead (e.g.
//! `http://localhost:8001`).
//!
//! ```bash
//! # Run against the in-process SQLite backend (default):
//! cargo test --test e2e
//!
//! # Run against a real server:
//! RESONATE_URL=http://localhost:8001 cargo test --test e2e
//! ```
//!
//! Every test runs on a multi-threaded runtime capped at `worker_threads = 2`:
//! these tests run in parallel, and an unbounded (per-CPU) runtime per test
//! oversubscribes the machine badly enough to starve reqwest's IO and blow the
//! 30s timeout when running against a real server.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use resonate_sdk::prelude::*;
use resonate_sdk::sqlite::{SqliteNetwork, SqliteServer};
use resonate_sdk::types::Value;

// ═══════════════════════════════════════════════════════════════════
//  Helpers
// ═══════════════════════════════════════════════════════════════════

/// Max number of tests allowed to hit a real server concurrently.
///
/// Each worker holds its own long-lived SSE/poll stream plus an HTTP client;
/// running all tests in parallel oversubscribes the machine's IO badly enough
/// that an occasional request stalls past the 30s timeout. Bounding concurrency
/// keeps real-server runs reliable. The in-process SQLite backend has no such
/// limit (each test is isolated and cheap), so it stays fully parallel.
const MAX_CONCURRENT_SERVER_TESTS: usize = 4;

/// Global gate limiting concurrent real-server tests. Held for a test's whole
/// lifetime via [`Backend::Url`].
fn server_gate() -> &'static tokio::sync::Semaphore {
    static GATE: tokio::sync::Semaphore =
        tokio::sync::Semaphore::const_new(MAX_CONCURRENT_SERVER_TESTS);
    &GATE
}

/// The server a test runs against.
///
/// `Url` targets a real Resonate server over HTTP (when `RESONATE_URL` is set)
/// and carries a concurrency-gate permit held for the test's lifetime; `Sqlite`
/// is an embedded in-process server. One `Backend` is created per test (via a
/// single `backend().await`) and shared by every `make_resonate` call in that
/// test, so multi-worker tests observe shared server state — and acquire the
/// gate exactly once — while staying isolated from other tests.
enum Backend {
    Url {
        url: String,
        _gate: tokio::sync::SemaphorePermit<'static>,
    },
    Sqlite(Arc<SqliteServer>),
}

/// Choose the backend for a test from the environment: a real server when
/// `RESONATE_URL` is set (acquiring a concurrency-gate permit), otherwise a
/// fresh in-process SQLite server.
async fn backend() -> Backend {
    match std::env::var("RESONATE_URL") {
        Ok(url) => {
            let gate = server_gate().acquire().await.expect("gate not closed");
            Backend::Url { url, _gate: gate }
        }
        Err(_) => {
            Backend::Sqlite(SqliteServer::open(":memory:").await.expect("open sqlite server"))
        }
    }
}

/// Generate a unique ID for a test run to avoid collisions on the server.
fn unique_id(test_name: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("e2e-{}-{}-{}", test_name, ts, n)
}

/// Default timeout applied to every server-facing await.
const E2E_TIMEOUT: Duration = Duration::from_secs(30);

/// Convenience wrapper: await a future with the default e2e timeout.
/// Accepts anything that implements `IntoFuture` (including the Resonate builders).
async fn with_timeout<F: std::future::IntoFuture>(f: F) -> F::Output {
    tokio::time::timeout(E2E_TIMEOUT, f.into_future())
        .await
        .expect("e2e test timed out")
}

/// Build a `Resonate` worker (unique pid/group) connected to `backend`.
fn make_resonate(backend: &Backend) -> Resonate {
    let pid = unique_id("worker");
    let group = unique_id("group");
    match backend {
        Backend::Url { url, .. } => Resonate::new(ResonateConfig {
            url: Some(url.clone()),
            pid: Some(pid),
            group: Some(group),
            ..Default::default()
        }),
        Backend::Sqlite(server) => {
            let net: Arc<SqliteNetwork> = Arc::new(server.connect(Some(pid), Some(group)));
            Resonate::new(ResonateConfig {
                network: Some(net),
                ..Default::default()
            })
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Test functions (leaf)
// ═══════════════════════════════════════════════════════════════════

#[resonate_sdk::function]
async fn add(x: i64, y: i64) -> Result<i64> {
    Ok(x + y)
}

#[resonate_sdk::function]
async fn greet(name: String) -> Result<String> {
    Ok(format!("hello, {}!", name))
}

#[resonate_sdk::function]
async fn noop() -> Result<()> {
    Ok(())
}

#[resonate_sdk::function]
async fn fail_always(msg: String) -> Result<String> {
    Err(Error::Application { message: msg })
}

#[resonate_sdk::function]
async fn hangs() -> Result<()> {
    std::future::pending::<()>().await;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
//  Test functions (workflows)
// ═══════════════════════════════════════════════════════════════════

#[resonate_sdk::function]
async fn sequential_workflow(ctx: &Context) -> Result<i64> {
    let a: i64 = ctx.rpc::<i64>("add", (1_i64, 2_i64)).await?;
    let b: i64 = ctx.rpc::<i64>("add", (a, 3_i64)).await?;
    Ok(b)
}

#[resonate_sdk::function]
async fn parallel_workflow(ctx: &Context) -> Result<i64> {
    let h1 = ctx.rpc::<i64>("add", (10_i64, 20_i64)).spawn()?;
    let h2 = ctx.rpc::<i64>("add", (30_i64, 40_i64)).spawn()?;
    let r1: i64 = h1.await?;
    let r2: i64 = h2.await?;
    Ok(r1 + r2)
}

#[resonate_sdk::function]
async fn run_sub_workflow(ctx: &Context) -> Result<i64> {
    let a: i64 = ctx.run(add, (5_i64, 5_i64)).await?;
    let b: i64 = ctx.run(add, (a, 10_i64)).await?;
    Ok(b)
}

// ═══════════════════════════════════════════════════════════════════
//  Tests — Basic connectivity & simple functions
// ═══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connectivity() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    let id = unique_id("connectivity");

    // Create and fetch a promise via the sub-client
    let created = with_timeout(
        r.promises
            .create(&id, i64::MAX, Value::default(), HashMap::new()),
    )
    .await;
    assert!(
        created.is_ok(),
        "should create promise: {:?}",
        created.err()
    );

    let fetched = with_timeout(r.promises.get(&id)).await;
    assert!(fetched.is_ok(), "should get promise: {:?}", fetched.err());
    assert_eq!(fetched.unwrap().id, id);

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simple_add() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(add).unwrap();

    let id = unique_id("simple-add");
    let result: i64 = with_timeout(r.run(&id, add, (3_i64, 4_i64))).await.unwrap();
    assert_eq!(result, 7);

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simple_greet() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(greet).unwrap();

    let id = unique_id("simple-greet");
    let result: String = with_timeout(r.run(&id, greet, "world".to_string()))
        .await
        .unwrap();
    assert_eq!(result, "hello, world!");

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simple_noop() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(noop).unwrap();

    let id = unique_id("simple-noop");
    let _: () = with_timeout(r.run(&id, noop, ())).await.unwrap();

    r.stop().await.unwrap();
}

// ═══════════════════════════════════════════════════════════════════
//  Tests — RPC & idempotency
// ═══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_to_registered_function() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(add).unwrap();

    let id = unique_id("rpc-add");
    let result: i64 = with_timeout(r.rpc(&id, "add", (10_i64, 20_i64)))
        .await
        .unwrap();
    assert_eq!(result, 30);

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idempotent_run() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(add).unwrap();

    let id = unique_id("idempotent-run");
    let r1: i64 = with_timeout(r.run(&id, add, (5_i64, 5_i64))).await.unwrap();
    let r2: i64 = with_timeout(r.run(&id, add, (5_i64, 5_i64))).await.unwrap();
    assert_eq!(r1, 10);
    assert_eq!(r2, 10);

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idempotent_rpc() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(add).unwrap();

    let id = unique_id("idempotent-rpc");
    let r1: i64 = with_timeout(r.rpc(&id, "add", (7_i64, 8_i64)))
        .await
        .unwrap();
    let r2: i64 = with_timeout(r.rpc(&id, "add", (7_i64, 8_i64)))
        .await
        .unwrap();
    assert_eq!(r1, 15);
    assert_eq!(r2, 15);

    r.stop().await.unwrap();
}

/// Replay a task whose worker is still holding it (simulates the
/// worker-died-mid-flight case from the user repro). The first worker
/// starts a task that hangs forever, leaving the task in `Acquired` state.
/// A second worker calls `run` with the same id; the server returns 409
/// because the task isn't (re-)acquirable. Pre-fix this surfaced as
/// `DecodingError("missing 'promise' in response")`; post-fix the SDK
/// subscribes to the existing promise and hands back a working handle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idempotent_run_orphaned_task() {
    let id = unique_id("orphaned");
    let backend = backend().await;

    let r_a = make_resonate(&backend);
    r_a.register(hangs).unwrap();
    let _h_a = r_a.run(&id, hangs, ()).spawn().await.unwrap();

    // Give the server a moment to ack the acquire before the second call.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let r_b = make_resonate(&backend);
    r_b.register(hangs).unwrap();
    let h_b = r_b.run(&id, hangs, ()).spawn().await;
    assert!(h_b.is_ok(), "expected handle on 409, got: {:?}", h_b.err());

    r_a.stop().await.unwrap();
    r_b.stop().await.unwrap();
}

/// RPC counterpart of `idempotent_run_orphaned_task`. The first worker
/// `run`s `hangs` (task acquired forever); a second client then `rpc`s the
/// same id. The rpc path goes through `promise.create` (idempotent on the
/// server), so the call should subscribe to the existing promise and hand
/// back a usable handle without surfacing a decoding/transport error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idempotent_rpc_orphaned_task() {
    let id = unique_id("orphaned-rpc");
    let backend = backend().await;

    let r_a = make_resonate(&backend);
    r_a.register(hangs).unwrap();
    let _h_a = r_a.run(&id, hangs, ()).spawn().await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let r_b = make_resonate(&backend);
    let h_b = r_b.rpc::<_, ()>(&id, "hangs", ()).spawn().await;
    assert!(h_b.is_ok(), "expected handle, got: {:?}", h_b.err());

    r_a.stop().await.unwrap();
    r_b.stop().await.unwrap();
}

/// A second worker calls `run` with an id whose promise has already been
/// resolved by a different worker. The server returns 409 (promise+task
/// already exist, task is fulfilled), and the SDK should subscribe and
/// hand back the cached value rather than re-executing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_after_resolved_different_worker() {
    let id = unique_id("run-after-resolved");
    let backend = backend().await;

    let r_a = make_resonate(&backend);
    r_a.register(add).unwrap();
    let r1: i64 = with_timeout(r_a.run(&id, add, (3_i64, 4_i64)))
        .await
        .unwrap();
    assert_eq!(r1, 7);

    let r_b = make_resonate(&backend);
    r_b.register(add).unwrap();
    let r2: i64 = with_timeout(r_b.run(&id, add, (3_i64, 4_i64)))
        .await
        .unwrap();
    assert_eq!(r2, 7);

    r_a.stop().await.unwrap();
    r_b.stop().await.unwrap();
}

/// Same as `run_after_resolved_different_worker`, but the first run failed.
/// The second worker should see the cached application error, not a
/// success or a transport-level error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_after_rejected_different_worker() {
    let id = unique_id("run-after-rejected");
    let backend = backend().await;

    let r_a = make_resonate(&backend);
    r_a.register(fail_always).unwrap();
    let r1: Result<String> = with_timeout(r_a.run(&id, fail_always, "boom".to_string())).await;
    assert!(r1.is_err(), "first run should fail");

    let r_b = make_resonate(&backend);
    r_b.register(fail_always).unwrap();
    let r2: Result<String> = with_timeout(r_b.run(&id, fail_always, "boom".to_string())).await;
    assert!(r2.is_err(), "second run should see the cached failure");

    r_a.stop().await.unwrap();
    r_b.stop().await.unwrap();
}

/// `run` completes, then `rpc` is invoked on the same id. The rpc path
/// goes through `promise.create` (idempotent), so it should resolve to
/// the cached value from the original `run`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_then_rpc_same_id() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(add).unwrap();

    let id = unique_id("run-then-rpc");
    let v1: i64 = with_timeout(r.run(&id, add, (1_i64, 2_i64))).await.unwrap();
    assert_eq!(v1, 3);

    let v2: i64 = with_timeout(r.rpc(&id, "add", (1_i64, 2_i64)))
        .await
        .unwrap();
    assert_eq!(v2, 3);

    r.stop().await.unwrap();
}

/// `rpc` completes, then `run` is invoked on the same id. The run path
/// goes through `task.create_or_conflict`; the server returns 409 because
/// the promise/task already exist, and the SDK should hand back the
/// cached value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_then_run_same_id() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(add).unwrap();

    let id = unique_id("rpc-then-run");
    let v1: i64 = with_timeout(r.rpc(&id, "add", (4_i64, 5_i64)))
        .await
        .unwrap();
    assert_eq!(v1, 9);

    let v2: i64 = with_timeout(r.run(&id, add, (4_i64, 5_i64))).await.unwrap();
    assert_eq!(v2, 9);

    r.stop().await.unwrap();
}

/// Two `run` calls with the same id fired in parallel from different
/// workers. One should win the create-and-acquire race; the other should
/// see 409 and subscribe. Both must observe the same result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_run_same_id() {
    let id = unique_id("concurrent-run");
    let backend = backend().await;

    let r_a = make_resonate(&backend);
    r_a.register(add).unwrap();
    let r_b = make_resonate(&backend);
    r_b.register(add).unwrap();

    let fut_a = with_timeout(r_a.run(&id, add, (11_i64, 22_i64)));
    let fut_b = with_timeout(r_b.run(&id, add, (11_i64, 22_i64)));
    let (v_a, v_b): (i64, i64) = tokio::join!(async { fut_a.await.unwrap() }, async {
        fut_b.await.unwrap()
    });
    assert_eq!(v_a, 33);
    assert_eq!(v_b, 33);

    r_a.stop().await.unwrap();
    r_b.stop().await.unwrap();
}

/// `run` is memoized by id, not by args. A second `run` with the same id
/// but different args should return the result the first invocation
/// produced — the new args must be silently ignored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_with_different_args_same_id_is_memoized() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(add).unwrap();

    let id = unique_id("memoized-args");
    let v1: i64 = with_timeout(r.run(&id, add, (1_i64, 1_i64))).await.unwrap();
    assert_eq!(v1, 2);

    let v2: i64 = with_timeout(r.run(&id, add, (5_i64, 5_i64))).await.unwrap();
    assert_eq!(
        v2, 2,
        "second run should reuse the first result, not re-execute"
    );

    r.stop().await.unwrap();
}

// ═══════════════════════════════════════════════════════════════════
//  Tests — Workflows with sub-calls and parallelism
// ═══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_sequential_rpcs() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(add).unwrap();
    r.register(sequential_workflow).unwrap();

    let id = unique_id("seq-workflow");
    let result: i64 = with_timeout(r.run(&id, sequential_workflow, ()))
        .await
        .unwrap();
    // 1+2=3, 3+3=6
    assert_eq!(result, 6);

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_parallel_rpcs() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(add).unwrap();
    r.register(parallel_workflow).unwrap();

    let id = unique_id("par-workflow");
    let result: i64 = with_timeout(r.run(&id, parallel_workflow, ()))
        .await
        .unwrap();
    // (10+20) + (30+40) = 100
    assert_eq!(result, 100);

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_with_ctx_run() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(add).unwrap();
    r.register(run_sub_workflow).unwrap();

    let id = unique_id("run-sub-workflow");
    let result: i64 = with_timeout(r.run(&id, run_sub_workflow, ()))
        .await
        .unwrap();
    // 5+5=10, 10+10=20
    assert_eq!(result, 20);

    r.stop().await.unwrap();
}

// ═══════════════════════════════════════════════════════════════════
//  Tests — Error propagation, handles, schedules
// ═══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn error_propagation() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(fail_always).unwrap();

    let id = unique_id("error-prop");
    let result: Result<String> = with_timeout(r.run(&id, fail_always, "boom".to_string())).await;
    assert!(result.is_err(), "should propagate error");

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_spawn_and_result() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    r.register(add).unwrap();

    let id = unique_id("handle-spawn");
    let handle = with_timeout(r.run(&id, add, (100_i64, 200_i64)).spawn())
        .await
        .unwrap();

    let result: i64 = with_timeout(handle.result()).await.unwrap();
    assert_eq!(result, 300);

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedule_create_and_delete() {
    let backend = backend().await;
    let r = make_resonate(&backend);

    let name = unique_id("schedule");
    let schedule = with_timeout(r.schedule(&name, "*/5 * * * *", "add", (1_i64, 2_i64)))
        .await
        .unwrap();

    let delete_result = with_timeout(schedule.delete()).await;
    assert!(
        delete_result.is_ok(),
        "should delete schedule: {:?}",
        delete_result.err()
    );

    r.stop().await.unwrap();
}

// ═══════════════════════════════════════════════════════════════════
//  Tests — Promises sub-client
// ═══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promises_resolve_roundtrip() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    let id = unique_id("promises-resolve");

    with_timeout(
        r.promises
            .create(&id, i64::MAX, Value::default(), HashMap::new()),
    )
    .await
    .unwrap();

    let payload = Value::from_serializable(serde_json::json!({"ok": true})).unwrap();
    let settled = with_timeout(r.promises.resolve(&id, payload))
        .await
        .unwrap();
    assert_eq!(settled.state, PromiseState::Resolved);

    let fetched = with_timeout(r.promises.get(&id)).await.unwrap();
    assert_eq!(fetched.state, PromiseState::Resolved);
    assert_eq!(
        fetched.value.data_as_ref(),
        &serde_json::json!({"ok": true})
    );

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promises_reject() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    let id = unique_id("promises-reject");

    with_timeout(
        r.promises
            .create(&id, i64::MAX, Value::default(), HashMap::new()),
    )
    .await
    .unwrap();

    let settled = with_timeout(r.promises.reject(&id, Value::default()))
        .await
        .unwrap();
    assert_eq!(settled.state, PromiseState::Rejected);

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promises_cancel() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    let id = unique_id("promises-cancel");

    with_timeout(
        r.promises
            .create(&id, i64::MAX, Value::default(), HashMap::new()),
    )
    .await
    .unwrap();

    let settled = with_timeout(r.promises.cancel(&id, Value::default()))
        .await
        .unwrap();
    assert_eq!(settled.state, PromiseState::RejectedCanceled);

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promises_get_not_found() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    let id = unique_id("promises-missing");

    let err = with_timeout(r.promises.get(&id)).await.unwrap_err();
    assert!(
        matches!(err, Error::ServerError { code: 404, .. }),
        "expected 404 ServerError, got {err:?}"
    );

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promises_create_is_idempotent() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    let id = unique_id("promises-conflict");

    let first = with_timeout(
        r.promises
            .create(&id, i64::MAX, Value::default(), HashMap::new()),
    )
    .await
    .unwrap();

    // Creating again with the same ID should return the original promise
    // (Resonate's create is idempotent / memoized by ID).
    let second = with_timeout(r.promises.create(
        &id,
        i64::MAX,
        Value::from_serializable(serde_json::json!({"different": true})).unwrap(),
        HashMap::new(),
    ))
    .await
    .expect("second create should succeed and return the existing promise");

    assert_eq!(second.id, first.id);
    assert_eq!(second.state, first.state);
    assert_eq!(second.created_at, first.created_at);

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promises_search_by_tag() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    let tag_value = unique_id("tag");
    let id1 = unique_id("search-a");
    let id2 = unique_id("search-b");

    let mut tags = HashMap::new();
    tags.insert("e2e".to_string(), tag_value.clone());

    with_timeout(
        r.promises
            .create(&id1, i64::MAX, Value::default(), tags.clone()),
    )
    .await
    .unwrap();
    with_timeout(
        r.promises
            .create(&id2, i64::MAX, Value::default(), tags.clone()),
    )
    .await
    .unwrap();

    let result = with_timeout(r.promises.search(None, Some(tags), Some(100), None))
        .await
        .unwrap();
    let ids: Vec<&String> = result.promises.iter().map(|p| &p.id).collect();
    assert!(ids.contains(&&id1), "expected {id1} in {ids:?}");
    assert!(ids.contains(&&id2), "expected {id2} in {ids:?}");

    r.stop().await.unwrap();
}

// ═══════════════════════════════════════════════════════════════════
//  Tests — Schedules sub-client
// ═══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedules_create_and_get() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    let id = unique_id("sched-create");
    let promise_tpl = format!("{id}.{{{{.timestamp}}}}");

    let created = with_timeout(r.schedules.create(
        &id,
        "*/5 * * * *",
        &promise_tpl,
        60_000,
        Value::default(),
    ))
    .await
    .unwrap();
    assert_eq!(created.id, id);
    assert_eq!(created.cron, "*/5 * * * *");

    let fetched = with_timeout(r.schedules.get(&id)).await.unwrap();
    assert_eq!(fetched.id, id);
    assert_eq!(fetched.cron, "*/5 * * * *");

    with_timeout(r.schedules.delete(&id)).await.unwrap();
    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedules_delete_not_found() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    let id = unique_id("sched-missing");

    let err = with_timeout(r.schedules.delete(&id)).await.unwrap_err();
    assert!(
        matches!(err, Error::ServerError { .. }),
        "expected ServerError, got {err:?}"
    );

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedules_search() {
    let backend = backend().await;
    let r = make_resonate(&backend);
    let id = unique_id("sched-search");
    let promise_tpl = format!("{id}.{{{{.timestamp}}}}");

    with_timeout(
        r.schedules
            .create(&id, "0 * * * *", &promise_tpl, 60_000, Value::default()),
    )
    .await
    .unwrap();

    let result = with_timeout(r.schedules.search(None, Some(100), None))
        .await
        .unwrap();
    assert!(
        result.schedules.iter().any(|s| s.id == id),
        "expected {id} in search results"
    );

    with_timeout(r.schedules.delete(&id)).await.unwrap();

    let after = with_timeout(r.schedules.search(None, Some(100), None))
        .await
        .unwrap();
    assert!(
        !after.schedules.iter().any(|s| s.id == id),
        "expected {id} absent after delete"
    );

    r.stop().await.unwrap();
}
