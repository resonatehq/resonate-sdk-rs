//! End-to-end tests for the embedded [`SqliteNetwork`].
//!
//! Unlike `e2e.rs` (which requires a running `resonate` server via
//! `RESONATE_URL`), these run entirely in-process against a SQLite database,
//! exercising the full execute / suspend / resume cycle through the vendored
//! server engine. They require a multi-threaded runtime because the SQLite
//! storage layer uses `tokio::task::block_in_place`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use resonate_sdk::prelude::*;
use resonate_sdk::sqlite::SqliteNetwork;
use resonate_sdk::types::Value;

fn unique_id(test_name: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("sqlite-{}-{}", test_name, n)
}

const TIMEOUT: Duration = Duration::from_secs(30);

async fn with_timeout<F: std::future::IntoFuture>(f: F) -> F::Output {
    tokio::time::timeout(TIMEOUT, f.into_future())
        .await
        .expect("test timed out")
}

/// Build a `Resonate` backed by an in-memory `SqliteNetwork`.
fn make_resonate() -> Resonate {
    let net = Arc::new(
        SqliteNetwork::new(":memory:", Some(unique_id("worker")), Some(unique_id("group")))
            .expect("open sqlite network"),
    );
    Resonate::new(ResonateConfig {
        network: Some(net),
        ..Default::default()
    })
}

// ── Functions ──────────────────────────────────────────────────────

#[resonate_sdk::function]
async fn add(x: i64, y: i64) -> Result<i64> {
    Ok(x + y)
}

#[resonate_sdk::function]
async fn greet(name: String) -> Result<String> {
    Ok(format!("hello, {}!", name))
}

#[resonate_sdk::function]
async fn sequential_workflow(ctx: &Context) -> Result<i64> {
    let a: i64 = ctx.rpc::<i64>("add", (1_i64, 2_i64)).await?;
    let b: i64 = ctx.rpc::<i64>("add", (a, 3_i64)).await?;
    Ok(b)
}

#[resonate_sdk::function]
async fn run_sub_workflow(ctx: &Context) -> Result<i64> {
    let a: i64 = ctx.run(add, (5_i64, 5_i64)).await?;
    let b: i64 = ctx.run(add, (a, 10_i64)).await?;
    Ok(b)
}

// ── Tests ──────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn connectivity() {
    let r = make_resonate();
    let id = unique_id("connectivity");

    let created = with_timeout(
        r.promises
            .create(&id, i64::MAX, Value::default(), HashMap::new()),
    )
    .await;
    assert!(created.is_ok(), "should create promise: {:?}", created.err());

    let fetched = with_timeout(r.promises.get(&id)).await;
    assert!(fetched.is_ok(), "should get promise: {:?}", fetched.err());
    assert_eq!(fetched.unwrap().id, id);

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn simple_add() {
    let r = make_resonate();
    r.register(add).unwrap();

    let id = unique_id("simple-add");
    let result: i64 = with_timeout(r.run(&id, add, (3_i64, 4_i64))).await.unwrap();
    assert_eq!(result, 7);

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn simple_greet() {
    let r = make_resonate();
    r.register(greet).unwrap();

    let id = unique_id("simple-greet");
    let result: String = with_timeout(r.run(&id, greet, "world".to_string()))
        .await
        .unwrap();
    assert_eq!(result, "hello, world!");

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_with_ctx_run() {
    let r = make_resonate();
    r.register(add).unwrap();
    r.register(run_sub_workflow).unwrap();

    let id = unique_id("ctx-run");
    let result: i64 = with_timeout(r.run(&id, run_sub_workflow, ()))
        .await
        .unwrap();
    assert_eq!(result, 20); // (5+5) then +10

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_sequential_rpcs() {
    let r = make_resonate();
    r.register(add).unwrap();
    r.register(sequential_workflow).unwrap();

    let id = unique_id("seq-rpc");
    let result: i64 = with_timeout(r.run(&id, sequential_workflow, ()))
        .await
        .unwrap();
    assert_eq!(result, 6); // (1+2) then +3

    r.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn run_is_idempotent() {
    let r = make_resonate();
    r.register(add).unwrap();

    let id = unique_id("idempotent");
    let r1: i64 = with_timeout(r.run(&id, add, (5_i64, 5_i64))).await.unwrap();
    let r2: i64 = with_timeout(r.run(&id, add, (5_i64, 5_i64))).await.unwrap();
    assert_eq!(r1, 10);
    assert_eq!(r2, 10);

    r.stop().await.unwrap();
}
