# Elide Zed extension

Rust → `wasm32-wasip2` via `zed_extension_api` 0.7. `cargo test` (native) covers the pure builders in `src/{lsp,dap,locator,komodo,project}.rs`; `src/e2e.rs` drives a real `elide` when `ELIDE_BIN` is set (`ELIDE_E2E_PYTHON=1` for Python).

## Lessons

- Keep host calls (`resolve_tcp_template`, `Worktree`, settings) in `src/lib.rs`; the wit imports are `unreachable!()` in native tests.
- Zed's TCP transport spawns `command` then retries the connect, and only connects when `command` is `None`: launch = command + connection, attach = connection only.
- Elide 1.6.3 honors DAP `stopOnEntry`; 1.6.0 always paused. Assert requested breakpoints, not entry-stop defaults. Canonicalize native fixture paths so `/var` versus `/private/var` does not prevent binding on macOS.
- Locators get the language's first debugger as `debug_adapter_name`, not ours; match on the task command (`elide run|test`) and always return adapter `Elide`.
- Extensions can only contribute `tasks.json` for languages they define, so run/test/build tasks are documented, not shipped.
- The registry's pinned `zed-extension` CLI (see CI `package` job) drops `opt_in_languages` and builds with Rust 1.90; keep code 1.90-compatible.
- Zed workspace trust gates language servers ("Waiting for worktree … to be trusted"); set `session.trust_all_worktrees` when testing headlessly.
- `pkill -f <pattern>` from the Bash tool matches the tool's own shell; use `pgrep -x`.
- e2e tests run in parallel: Elide's DAP server serves one client and silently closes others, so a reused port looks like "adapter closed the connection"; allocate via `free_port()`.
- Elide may close the socket instead of answering `disconnect {terminateDebuggee: true}`; don't wait for that response.
- The `zed-extension` CLI writes `extension.wasm` into the source dir (gitignored).
- Komodo DAP uses stdio; its attach `port` is a JDWP endpoint, never `tcp_connection`. LSP argument overrides must not replace DAP transport arguments.
- Komodo models are generated in extension storage on startup/restart. LSP skips explicit Java-only roots; DAP keeps them. No host workspace writes, implicit installs, or background sync.
- Zed's `lsp.<server>.binary.path` bypasses `language_server_command` entirely, defaulting to empty argv. Komodo must use `settings.komodoPath`; Java binary overrides must include `arguments: ["lsp"]`.
- Every `zed::process::Command` needs a manifest `process:exec` capability. Verify the packaged WASM in an isolated Zed `--user-data-dir`, not only native builders. Put smoke settings in `<dir>/config/settings.json`.
- Android framework/R classes are on `<sourceSet>:provided`, not `:compile`; merge both for analysis. Actual Zed smoke must reach `didOpen` and diagnostics, not merely process startup.
- Manual Cargo builds must copy `target/wasm32-wasip2/release/zed_elide.wasm` to root `extension.wasm`; the installed dev-extension symlink loads the root file.
- A dev extension's manifest is cached in `extensions/index.json`; changing the symlink target's manifest and restarting can retain stale capabilities. Verify the normal profile's cached manifest, not just an isolated profile.
