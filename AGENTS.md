# resonate-sdk-rs — Agent Orientation

> **Read the README first for the user-facing overview.** This file is the agent orientation: what to know before editing the SDK source. Voice = Echo. The Rust SDK ships fast — most-active SDK in the repo set right now.

The Rust SDK for [Resonate](https://resonatehq.io). Targets the v0.9.x Rust server. Published as [`resonate-sdk`](https://crates.io/crates/resonate-sdk) on crates.io; also installable as a git dependency for tracking `main`.

## Status

- **Latest published:** `resonate-sdk` 0.4.0 on crates.io (2026-04-28). Published 0.1.0, 0.3.0, 0.4.0; 0.2.0 is GitHub-release-only.
- **Cadence:** very active. Andres lands near-daily commits; releases come in clusters of 2-3 per week.
- **API surface:** post-rename (PR #7 merged 2026-04-15) — promise API uses `.resolve()` / `.reject()` / `.cancel()`. The pre-rename `.settle()` shape is gone.
- **Owner:** Andres Villegas (avillega)

## Stack

| | |
|---|---|
| Toolchain | Rust stable (no MSRV documented yet) |
| Runtime | tokio (full features) |
| Build | **cargo** workspace (`resonate` + `resonate-macros`) |
| Linter / formatter | **rustfmt** + **clippy** |
| Tests | `cargo test` (unit per crate + workspace e2e in `resonate/tests/e2e.rs`) |
| Macros | `resonate-macros` provides `#[resonate_sdk::function]` |
| License | Apache-2.0 |

A more granular stack table lives in this repo's README; only restate it here when an agent needs the toolchain trio at a glance.

## Run

```bash
cargo build               # build the workspace
cargo build --release     # release build
cargo test                # run all tests (some require a running server — see below)
cargo fmt --check         # rustfmt check
cargo fmt                 # rustfmt apply
cargo clippy --all-targets --all-features  # clippy lints
```

The end-to-end tests in `resonate/tests/e2e.rs` use `test-with` to gate on a running Resonate server. Run a server first (`brew install resonatehq/tap/resonate && resonate dev`) to exercise the full integration matrix; otherwise unit tests run alone.

Cully prefers to run long-lived processes himself. Run one-shot commands (build, test, fmt, clippy) freely; don't auto-start a server or watcher.

## Architecture notes

- **Workspace layout:** `resonate/` is the SDK crate; `resonate-macros/` is the proc-macro crate that owns `#[resonate_sdk::function]`. Users depend on `resonate-sdk`; the macro crate is a transitive dep.
- **Entry point** is `src/resonate.rs` — the `Resonate` struct users construct via `Resonate::new(config)` (server-backed) or `Resonate::local()` (in-memory). `register()` takes a function annotated with `#[resonate_sdk::function]` and adds it to the registry in `src/registry.rs`.
- **Function kinds** are detected by the macro from the first parameter:
  - `&Context` → workflow function (can spawn sub-tasks via `ctx.run()` / `ctx.rpc()` / `ctx.sleep()`)
  - `&Info` → leaf with read-only metadata access
  - anything else → pure leaf
- **Durable execution loop** lives across `src/core.rs` (engine), `src/transport.rs` (server I/O), and `src/futures.rs` (Resonate's custom Future implementations that integrate with tokio).
- **Promise primitives** in `src/promises.rs` — durable promises backed by the server. The v0.4.0 API surface is `.resolve()` / `.reject()` / `.cancel()` after the rename.
- **Network / transport** is `src/http_network.rs` + `src/transport.rs`. Protocol version is set in `src/lib.rs` (`PROTOCOL_VERSION = "2026-04-01"`). Server protocol changes ripple here.
- **Builders** (returned by `ctx.run()`, `ctx.rpc()`, `resonate.run()`, etc.) support `.timeout()`, `.target()`, plus client-side `.version()` and `.tags()`. Both sequential `.await` and parallel `.spawn().await` shapes are supported.

## Rules

1. **No `cargo publish` without explicit Cully approval** for that specific publish, in the same session. Andres handles publishes; agents draft, don't ship.
2. **No pushing to `main` directly.** Open a PR; CI runs the test matrix.
3. **Workspace versions are coupled.** `resonate` and `resonate-macros` ship together via the workspace `version.workspace = true`; the `bump-version.sh` script keeps them aligned.
4. **Server protocol is upstream.** `PROTOCOL_VERSION` in `src/lib.rs` matches the targeted server release; don't bump it without coordinating against the server repo.
5. **Voice in user-visible strings = Echo.** Error variants in `src/error.rs`, log lines, and rustdoc all use the Echo voice (technical, precise, friendly-but-not-casual).

## Pointers

- [README](./README.md) — quickstart and external-facing overview
- [Rust SDK guide on docs.resonatehq.io](https://docs.resonatehq.io/develop/rust) — user-facing docs for the SDK
- [Resonate Server (Rust)](https://github.com/resonatehq/resonate) — the server this SDK targets
- [Example apps](https://github.com/resonatehq-examples) — `example-*-rs` repos demonstrate end-to-end patterns
- [crates.io listing](https://crates.io/crates/resonate-sdk)
- [Distributed Async Await](https://www.distributed-async-await.io/) — the underlying programming model

## Known gaps

- **MSRV is not documented.** The workspace `Cargo.toml` doesn't pin a `rust-version`. Stable toolchain is the assumption; pin when the SDK starts depending on a specific stable release feature.
- **README shows `resonate-sdk = "0.1"`.** Now stale — 0.4.0 is current on crates.io. README install snippet refresh is queued for the next docs sweep.
- **No public API reference site.** rustdoc builds locally (`cargo doc`) but is not published outside docs.rs (which auto-builds from the crates.io release).
