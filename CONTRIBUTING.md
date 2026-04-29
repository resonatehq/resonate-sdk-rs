# Contribute to the Resonate Rust SDK

Please [open a GitHub Issue](https://github.com/resonatehq/resonate-sdk-rs/issues) prior to submitting a Pull Request.

Join the `#resonate-engineering` channel in the [community Discord](https://resonatehq.io/discord) to discuss your changes.

## Toolchain

Install the current stable Rust toolchain via [rustup](https://rustup.rs/).

## Build, test, lint

```shell
cargo build --workspace
cargo test --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

CI gates merging on `cargo fmt --all --check` (no in-place rewrite), `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`. Run `cargo fmt --all` locally before pushing so the `--check` gate passes. See [`.github/workflows/ci.yml`](./.github/workflows/ci.yml) for the full workflow.
