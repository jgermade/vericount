# vericounts

Bookkeeping for a Spanish SL with Veri*factu compliance. See the latest roadmap in [`ROADMAP/`](ROADMAP/) and the agent conventions in [`AGENTS.md`](AGENTS.md).

## Development

Requires Rust (the version is pinned in `rust-toolchain.toml`).

```bash
cargo run                 # serves web/ on http://127.0.0.1:8080
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

Configuration (environment variables):

| Variable | Default | Purpose |
|---|---|---|
| `VERICOUNTS_ADDR` | `127.0.0.1:8080` | Listen address |
| `VERICOUNTS_WEB_DIR` | `web` | Directory with the static web client |

## Layout

- `crates/server`: Axum HTTP server, security headers and their tests.
- `web`: static web client (no inline scripts, styles or event handlers).
