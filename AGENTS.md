# AGENTS

Project: `chatterd` (Matrix-only daemon for Chatter).

## Goals
- Keep `chatterd` as a daemon with a JSON-RPC control surface.
- UI drives account creation, homeserver discovery, and login flows.
- Do not store user credentials in config; use state storage.

## Repo layout
- `src/main.rs`: daemon entrypoint + JSON-RPC wiring.
- `src/config.rs`: daemon config loading/defaults.
- `src/state.rs`: account/state types + persistence.
- `src/matrix.rs`: Matrix SDK helpers + runtime.
- `src/rpc.rs`: JSON-RPC server and Matrix handlers.
- `docs/jsonrpc.md`: JSON-RPC method reference.
- `chatterd.toml` (optional): daemon prefs only.

## Config
- `CHATTERD_CONFIG` env var overrides config file path (default: `chatterd.toml`).
- Config contains only daemon prefs (`state_dir`, `rpc_listen`).
- Accounts are stored in `state_dir/accounts.json`.

## JSON-RPC
- Transport: TCP, newline-delimited JSON.
- Default listen: `127.0.0.1:9388`.
- Matrix discovery uses spec-typed responses and falls back to raw JSON with `parse_warning`.

## Build/test
- Build: `cargo build`
- Run: `cargo run`
- Quick RPC test:
  - `printf '{"jsonrpc":"2.0","id":1,"method":"rpc.ping"}\n' | nc 127.0.0.1 9388`

## Notes for agents
- Prefer spec-correct Matrix handling via `ruma` types.
- Avoid adding UI concerns; keep daemon headless.
- Keep ASCII-only edits unless the file already uses Unicode.
