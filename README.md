# Elide for Zed

[Elide](https://elide.dev) support for the [Zed](https://zed.dev) editor:

- **Language server**: `elide lsp` for Java (diagnostics, completion, hover, navigation, rename, formatting, code
  actions, inlay hints, semantic tokens).
- **Debugger**: an `Elide` debug adapter for JavaScript, TypeScript and Python. It launches
  `elide run --debugger=dap` (or `elide test`) and connects to it, or attaches to one you started yourself.
- **Debug from tasks**: any Zed task that runs `elide run …` or `elide test …` shows up in the debugger's
  **Debug** tab.

![Debugging app.js with Elide in Zed](assets/debug.png)

## Requirements

- Elide 1.6.0 or newer on `PATH`, or set its path in settings (below). Python debugging needs an Elide build with
  Python.
- The [Java](https://github.com/zed-extensions/java) extension, which defines the Java language in Zed.
- For `elide.pkl` highlighting, the [Pkl](https://github.com/Moshyfawn/pkl-zed) extension. This extension does not
  ship a Pkl grammar of its own.

## Install

Until the extension is in Zed's registry, install it as a dev extension (needs Rust via `rustup`):

```bash
git clone https://github.com/elide-dev/zed elide-zed
```

In Zed, open the Extensions page, click **Install Dev Extension** and select the `elide-zed` directory.

## Java language server

Name `elide` in the Java `language_servers` list. To use it instead of jdtls:

```json
{
  "languages": {
    "Java": {
      "language_servers": ["elide", "!jdtls", "..."]
    }
  }
}
```

Keep both with `["elide", "jdtls"]`, or turn Elide off with `["!elide", "..."]`. Zed builds that support
`opt_in_languages` start `elide` for Java only when you name it; older builds start it next to jdtls.

`elide lsp` reads the workspace root Zed sends; an `elide.pkl` there marks an Elide project.

To use a specific binary or extra flags:

```json
{
  "lsp": {
    "elide": {
      "binary": {
        "path": "/opt/elide/bin/elide",
        "arguments": ["lsp"],
        "env": { "NO_COLOR": "1" }
      }
    }
  }
}
```

`arguments` replaces the default `["lsp"]`. `initialization_options` and `settings` are passed to the server as-is.

## Debugger

Open the debugger with `debugger: start` (F4). Pick a scenario from `.zed/debug.json`, a task in the **Debug** tab,
or **Launch** a program with the `Elide` adapter.

### `.zed/debug.json`

```json
[
  {
    "label": "Debug app.ts",
    "adapter": "Elide",
    "request": "launch",
    "program": "app.ts",
    "args": ["--port", "8080"],
    "env": { "DEBUG": "1" }
  },
  {
    "label": "Debug the current file",
    "adapter": "Elide",
    "request": "launch",
    "program": "$ZED_FILE"
  },
  {
    "label": "Debug tests",
    "adapter": "Elide",
    "request": "launch",
    "command": "test",
    "args": ["sample.test.mts"]
  },
  {
    "label": "Attach to elide on 4711",
    "adapter": "Elide",
    "request": "attach",
    "tcp_connection": { "host": "127.0.0.1", "port": 4711 }
  }
]
```

Launch options:

| Field | Meaning |
| --- | --- |
| `program` | File to run, relative to `cwd`. |
| `args` | Arguments after `program`. Without `program`, passed to the subcommand as-is. |
| `cwd` | Working directory; defaults to the worktree root. |
| `env` | Extra environment variables. |
| `command` | `run` (default) or `test`. |
| `elideArgs` | Elide flags placed before `program`, such as `["--max-mem=512m"]`. |
| `host`, `port` | DAP listener address; defaults to `127.0.0.1` and a free port. |

The adapter runs
`elide <command> --debugger=dap --debugger-host=<host> --debugger-port=<port> [elideArgs] [program] [args]`
and connects once the port opens. Program output appears in the debug console.

For `attach`, start the program yourself and point Zed at it with `tcp_connection` or `host`/`port`
(default `127.0.0.1:4711`):

```bash
elide run --debugger=dap app.py
```

To use a specific `elide` for debugging:

```json
{ "dap": { "Elide": { "binary": "/opt/elide/bin/elide" } } }
```

To make Elide the default debugger for a language's gutter and new-process modal:

```json
{ "languages": { "TypeScript": { "debuggers": ["Elide"] } } }
```

### Known limitations (Elide 1.6.0)

- Elide pauses on entry before the first statement; `stopOnEntry` has no effect. Continue (F5) to reach your
  breakpoints.
- TypeScript breakpoints do not bind yet; JavaScript and Python breakpoints do.
- In Python, the entry pause is inside the standard library's `importlib`.
- Java and Kotlin debugging (JDWP) is not offered: Zed only debugs Java through the Java extension's jdtls plugin.

## Tasks

Zed extensions can only add tasks to languages they define, so add Elide tasks to `.zed/tasks.json`:

```json
[
  { "label": "elide run $ZED_FILE", "command": "elide", "args": ["run", "$ZED_FILE"] },
  { "label": "elide test", "command": "elide", "args": ["test"] },
  { "label": "elide build", "command": "elide", "args": ["build"] }
]
```

The `run` and `test` tasks can also be debugged from the **Debug** tab of `debugger: start`.

## Development

```bash
cargo test                                   # unit tests
ELIDE_BIN=$(which elide) cargo test e2e      # end-to-end against a real elide
ELIDE_E2E_PYTHON=1 ELIDE_BIN=… cargo test e2e  # also debug Python (needs an Elide build with Python)
cargo build --release --target wasm32-wasip2
```

The end-to-end tests build the same launch command the extension gives Zed, run it, and drive a DAP session to a
breakpoint; they also start `elide lsp` and request a hover.

## License

MIT
