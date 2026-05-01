# resonate-sdk-rs — Agent Orientation

> **Read the README first for the user-facing overview.** This file is for agents *editing this repo*, not for agents *consuming the SDK in an app*. README is the consumer surface; AGENTS.md is the developer surface.

The Rust SDK for [Resonate](https://resonatehq.io). Tracks the v0.9.x Rust server; protocol coupling lives at `PROTOCOL_VERSION` in `resonate/src/lib.rs`. Most-active SDK in the repo set right now.

## Status

- **Active version line:** 0.4.x — pre-1.0, so breaking changes are acceptable with rationale. The next published bump runs through `bump-version.sh`. Releases land on crates.io as [`resonate-sdk`](https://crates.io/crates/resonate-sdk) (currently 0.4.0, 2026-04-28).
- **Commit cadence is high** — near-daily merges, releases in clusters of 2-3/week. Keep PRs small and focused; long-lived branches accumulate conflict fast against `main`.
- **Codebase is post-rename.** PR #7 (merged 2026-04-15) replaced `.settle()` with `.resolve()` / `.reject()` / `.cancel()` on the promise API. **Do not reintroduce the `.settle()` shape** in new code, examples, or rustdoc.

## Stack

| | |
|---|---|
| Toolchain | Rust stable (no MSRV pinned — see Known gaps) |
| Runtime | `tokio = { features = ["full"] }` |
| Build | **cargo** workspace (`resonate/` + `resonate-macros/`) |
| Linter / formatter | **rustfmt** + **clippy** |
| Tests | `cargo test` (unit per crate + workspace e2e in `resonate/tests/e2e.rs`) |
| Macros | crate `resonate-sdk-macros` (dir: `resonate-macros/`) provides `#[resonate_sdk::function]` |
| License | Apache-2.0 |

## Run

```bash
cargo build                                # build the workspace
cargo build --release                      # release build
cargo test                                 # unit tests (e2e silently skipped without RESONATE_URL — see below)
cargo fmt --check                          # rustfmt check
cargo fmt                                  # rustfmt apply
cargo clippy --all-targets --all-features  # clippy lints
```

End-to-end tests in `resonate/tests/e2e.rs` are gated by `#[test_with::env(RESONATE_URL)]`. **`cargo test` alone will silently skip them** — running a local server is not enough; `RESONATE_URL` must be set in the same shell:

```bash
brew install resonatehq/tap/resonate           # one-time
resonate dev &                                 # start the server
RESONATE_URL=http://localhost:8001 cargo test --test e2e
```

Run one-shot commands (build, test, fmt, clippy) freely; don't auto-start a server or watcher — the operator runs long-lived processes themselves.

## Architecture notes

- **Workspace layout:** `resonate/` is the SDK crate; `resonate-macros/` is the proc-macro crate that owns `#[resonate_sdk::function]`. Consumers depend on `resonate-sdk`; the macro crate is a transitive dep.
- **Entry point** is `resonate/src/resonate.rs` — the `Resonate` struct, constructed via `Resonate::new(config)` (server-backed) or `Resonate::local()` (in-memory). `register()` accepts a function annotated with `#[resonate_sdk::function]` and adds it to the registry in `resonate/src/registry.rs`.
- **Function kinds** are detected by the macro from the first parameter:
  - `&Context` → workflow function (can spawn sub-tasks via `ctx.run()` / `ctx.rpc()` / `ctx.sleep()`)
  - `&Info` → leaf with read-only metadata access
  - anything else → pure leaf
- **Durable execution loop** lives across `resonate/src/core.rs` (engine), `resonate/src/transport.rs` (server I/O), and `resonate/src/futures.rs` (custom Future implementations that integrate with tokio).
- **Promise primitives** in `resonate/src/promises.rs` — durable promises backed by the server. The post-rename surface is `.resolve()` / `.reject()` / `.cancel()`.
- **Network / transport** is `resonate/src/http_network.rs` + `resonate/src/transport.rs`. `PROTOCOL_VERSION` in `resonate/src/lib.rs` is the wire-protocol pin to the server; server protocol changes ripple here.
- **Builders** (returned by `ctx.run()`, `ctx.rpc()`, `resonate.run()`, etc.) support `.timeout()`, `.target()`, plus client-side `.version()` and `.tags()`. Both sequential `.await` and parallel `.spawn().await` shapes are supported.

## Rules

1. **No `cargo publish` without explicit maintainer approval** for that specific publish, in the same session. The release workflow handles publishes; agents draft, don't ship.
2. **No pushing to `main` directly.** Open a PR; CI runs the test matrix.
3. **The two crates version independently.** `resonate-sdk` uses `version.workspace = true` and follows `bump-version.sh`; `resonate-sdk-macros` is pinned in its own `Cargo.toml` (currently `0.1.0`) and `resonate-sdk` depends on it by exact version. When the macro surface changes, bump `resonate-sdk-macros` by hand and update the dependency pin in `resonate/Cargo.toml`.
4. **Server protocol is upstream.** `PROTOCOL_VERSION` matches the targeted server release; don't bump it without coordinating against the server repo.
5. **Voice in user-visible strings = Echo.** Error variants in `resonate/src/error.rs`, log lines, and rustdoc all use the Echo voice (technical, precise, friendly-but-not-casual).

## Pointers

- [README](./README.md) — the consumer-facing surface this SDK ships; check it when changing the public API
- [Resonate Server (Rust)](https://github.com/resonatehq/resonate) — the server this SDK targets; protocol changes originate here
- [Rust SDK guide on docs.resonatehq.io](https://docs.resonatehq.io/develop/rust) — public docs surface; verify behavior matches when changing public API
- [Example apps](https://github.com/resonatehq-examples) — `example-*-rs` repos exercise end-to-end patterns; useful for catching consumer-visible regressions
- [Distributed Async Await](https://www.distributed-async-await.io/) — the programming-model spec the SDK implements

## Privacy

- The SDK accepts bearer tokens via `RESONATE_TOKEN` (env) or builder/config arg. Tokens flow through the network layer; **never log them, never echo them back in error messages.**
- Application-level secret handling belongs in the encoder/codec layer, not in ad-hoc plumbing across the SDK.

## Known gaps

- **MSRV is not documented.** The workspace `Cargo.toml` doesn't pin a `rust-version`. Stable is the assumption; pin when the SDK starts depending on a specific stable release feature.
- **README install snippet shows `resonate-sdk = "0.1"`.** Stale; 0.4.0 is current. Refresh queued for the next docs sweep (tracked under `readme-curation`, not this repo).
- **No public API reference site.** `cargo doc` builds locally; published rustdoc lives only at docs.rs (auto-built from the crates.io release).
