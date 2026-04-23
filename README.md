# Resonate Rust SDK

[![ci](https://github.com/resonatehq/resonate-sdk-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/resonatehq/resonate-sdk-rs/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

> **Early development.** The Rust SDK is v0.1.0 and not yet published on crates.io. APIs may change between releases.

## About this component

The Resonate Rust SDK enables developers to build reliable and scalable cloud applications in Rust. Built on [tokio](https://tokio.rs), it gives you durable execution with automatic recovery, idempotency, and distributed coordination.

- [How to contribute to this SDK](./CONTRIBUTING.md)
- [Evaluate Resonate for your next project](https://docs.resonatehq.io/evaluate/)
- [Example application library](https://github.com/resonatehq-examples)
- [Distributed Async Await — the concepts that power Resonate](https://www.distributed-async-await.io/)
- [Rust SDK guide on docs.resonatehq.io](https://docs.resonatehq.io/develop/rust)
- [Join the Discord](https://resonatehq.io/discord)
- [Subscribe to the Journal](https://journal.resonatehq.io/subscribe)
- [Follow on X](https://x.com/resonatehqio)
- [Follow on LinkedIn](https://www.linkedin.com/company/resonatehqio)
- [Subscribe on YouTube](https://www.youtube.com/@resonatehqio)

## Quickstart

1. Install the Resonate Server & CLI

```shell
brew install resonatehq/tap/resonate
```

2. Install the Resonate SDK

Because the crate is not yet published on crates.io, add it as a git dependency:

```toml
[dependencies]
resonate-sdk = { git = "https://github.com/resonatehq/resonate-sdk-rs", branch = "main" }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
```

3. Write your first Resonate Function

A greeting as a durable workflow. Simple, but the function resumes cleanly from the last checkpoint if the process crashes mid-execution.

```rust
use resonate_sdk::prelude::*;

// A workflow function — receives &Context for durable sub-task orchestration.
#[resonate_sdk::function]
async fn greet(ctx: &Context, name: String) -> Result<String> {
    let greeting = ctx.run(format_greeting, name).await?;
    Ok(greeting)
}

// A leaf function — pure computation, no Context needed.
#[resonate_sdk::function]
async fn format_greeting(name: String) -> Result<String> {
    Ok(format!("Hello, {}!", name))
}

#[tokio::main]
async fn main() {
    let resonate = Resonate::new(ResonateConfig {
        url: Some("http://localhost:8001".into()),
        ..Default::default()
    });
    resonate.register(greet).unwrap();
    resonate.register(format_greeting).unwrap();

    let result: String = resonate
        .run("greet-1", greet, "World".into())
        .await
        .expect("workflow failed");

    println!("{result}");
}
```

[Clone a working example repo](https://github.com/resonatehq-examples/example-hello-world-rs)

4. Start the server

```shell
resonate dev
```

5. Start the worker

```shell
cargo run
```

6. Run the function

Run the function with execution ID `greet-1`:

```shell
resonate invoke greet-1 --func greet --arg "World"
```

**Result**

You will see the greeting in the terminal:

```shell
Hello, World!
```

**What to try**

Try killing the worker mid-execution and restarting. **The workflow resumes from its last durable checkpoint without losing progress.**

For the full API reference, including entry-point methods, client APIs, Context APIs, builder options, and function annotations, see the [Rust SDK guide](https://docs.resonatehq.io/develop/rust).

## Environment variables

| Variable | Description | Default |
|---|---|---|
| `RESONATE_URL` | Full server URL | *(local mode)* |
| `RESONATE_HOST` | Server hostname | *(unset)* |
| `RESONATE_PORT` | Server port | `8001` |
| `RESONATE_TOKEN` | JWT auth token | *(unset)* |
| `RESONATE_PREFIX` | Promise ID prefix | *(empty)* |

Constructor arguments take precedence over environment variables. If no URL is configured, the SDK runs in local mode (in-memory, no server required).

## License

Apache-2.0 — see [LICENSE](./LICENSE).
