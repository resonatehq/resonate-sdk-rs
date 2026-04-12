//! Integration tests for application state (`with_state` / `ctx.state::<T>()`).

use resonate::prelude::*;

struct MyState {
    greeting: String,
}

#[resonate::function]
async fn read_state(ctx: &Context) -> Result<String> {
    let s = ctx.state::<MyState>();
    Ok(s.greeting.clone())
}

#[resonate::function]
async fn workflow_reads_state(ctx: &Context) -> Result<String> {
    let greeting: String = ctx.run(read_state, ()).await?;
    Ok(format!("workflow says: {}", greeting))
}

#[tokio::test]
async fn leaf_function_accesses_state() {
    let r = Resonate::local()
        .with_state(MyState { greeting: "hello".to_string() });
    r.register(read_state).unwrap();

    let result: String = r.run("leaf-state-test", read_state, ()).await.unwrap();
    assert_eq!(result, "hello");

    r.stop().await.unwrap();
}

#[tokio::test]
async fn workflow_accesses_state_via_child() {
    let r = Resonate::local()
        .with_state(MyState { greeting: "hi".to_string() });
    r.register(read_state).unwrap();
    r.register(workflow_reads_state).unwrap();

    let result: String = r.run("workflow-state-test", workflow_reads_state, ()).await.unwrap();
    assert_eq!(result, "workflow says: hi");

    r.stop().await.unwrap();
}

#[tokio::test]
async fn multiple_state_types() {
    struct DbConn(String);
    struct AppConfig(u32);

    #[resonate::function]
    async fn read_both(ctx: &Context) -> Result<String> {
        let db = ctx.state::<DbConn>();
        let cfg = ctx.state::<AppConfig>();
        Ok(format!("{}:{}", db.0, cfg.0))
    }

    let r = Resonate::local()
        .with_state(DbConn("localhost".to_string()))
        .with_state(AppConfig(8080));
    r.register(read_both).unwrap();

    let result: String = r.run("multi-state-test", read_both, ()).await.unwrap();
    assert_eq!(result, "localhost:8080");

    r.stop().await.unwrap();
}
