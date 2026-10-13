# Elide Zed extension

Rust → `wasm32-wasip2` via `zed_extension_api` 0.7. `cargo test` (native) covers the pure builders in `src/{lsp,dap,locator}.rs`; `src/e2e.rs` drives a real `elide` when `ELIDE_BIN` is set (`ELIDE_E2E_PYTHON=1` for Python).

## Lessons

- Keep host calls (`resolve_tcp_template`, `Worktree`, settings) in `src/lib.rs`; the wit imports are `unreachable!()` in native tests.
- Zed's TCP transport spawns `command` then retries the connect, and only connects when `command` is `None`: launch = command + connection, attach = connection only.
- Elide's DAP accepts both `launch` and `attach` and always pauses on entry; there is no stop-on-entry switch.
- Locators get the language's first debugger as `debug_adapter_name`, not ours; match on the task command (`elide run|test`) and always return adapter `Elide`.
- Extensions can only contribute `tasks.json` for languages they define, so run/test/build tasks are documented, not shipped.
- The registry's pinned `zed-extension` CLI (see CI `package` job) drops `opt_in_languages` and builds with Rust 1.90; keep code 1.90-compatible.
- Zed workspace trust gates language servers ("Waiting for worktree … to be trusted"); set `session.trust_all_worktrees` when testing headlessly.
- `pkill -f <pattern>` from the Bash tool matches the tool's own shell; use `pgrep -x`.
- e2e tests run in parallel: Elide's DAP server serves one client and silently closes others, so a reused port looks like "adapter closed the connection"; allocate via `free_port()`.
- Elide may close the socket instead of answering `disconnect {terminateDebuggee: true}`; don't wait for that response.
- The `zed-extension` CLI writes `extension.wasm` into the source dir (gitignored).
