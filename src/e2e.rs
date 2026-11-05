//! End-to-end checks against a real `elide`; set `ELIDE_BIN` (and `ELIDE_E2E_PYTHON=1` for a Python-enabled build).

use std::{
  io::{BufRead, BufReader, Write},
  net::{Ipv4Addr, TcpListener, TcpStream},
  path::{Path, PathBuf},
  process::{Child, Command, Stdio},
  time::{Duration, Instant},
};

use serde_json::{Value, json};
use zed_extension_api::{StartDebuggingRequestArgumentsRequest, TaskTemplate, TcpArguments};

use crate::{dap, locator, lsp};

fn elide() -> Option<String> {
  std::env::var("ELIDE_BIN").ok().filter(|s| !s.is_empty())
}

fn fixture_dir(name: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("zed-elide-e2e-{}-{name}", std::process::id()));
  std::fs::create_dir_all(&dir).unwrap();
  dir.canonicalize().unwrap()
}

fn free_port() -> u16 {
  TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
    .unwrap()
    .local_addr()
    .unwrap()
    .port()
}

struct Killer(Child);

impl Drop for Killer {
  fn drop(&mut self) {
    let _ = self.0.kill();
    let _ = self.0.wait();
  }
}

fn read_framed(r: &mut impl BufRead) -> Option<Value> {
  let mut len = None;
  loop {
    let mut line = String::new();
    if r.read_line(&mut line).ok()? == 0 {
      return None;
    }
    let line = line.trim();
    if line.is_empty() {
      break;
    }
    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
      len = v.trim().parse().ok();
    }
  }
  let mut buf = vec![0; len?];
  r.read_exact(&mut buf).ok()?;
  serde_json::from_slice(&buf).ok()
}

fn write_framed(w: &mut impl Write, v: &Value) {
  let body = v.to_string();
  write!(w, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
  w.flush().unwrap();
}

struct Dap {
  reader: BufReader<TcpStream>,
  writer: TcpStream,
  seq: i64,
  seen: Vec<String>,
}

impl Dap {
  fn connect(port: u16, child: &mut Child) -> Self {
    let start = Instant::now();
    let stream = loop {
      match TcpStream::connect((Ipv4Addr::LOCALHOST, port)) {
        Ok(s) => break s,
        Err(_) => {
          if let Ok(Some(status)) = child.try_wait() {
            panic!("elide exited before the debugger attached: {status}");
          }
          assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out connecting to {port}"
          );
          std::thread::sleep(Duration::from_millis(100));
        }
      }
    };
    stream.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    Self {
      reader: BufReader::new(stream.try_clone().unwrap()),
      writer: stream,
      seq: 0,
      seen: vec![],
    }
  }

  fn send(&mut self, command: &str, arguments: Value) -> i64 {
    self.seq += 1;
    let msg = json!({"seq": self.seq, "type": "request", "command": command, "arguments": arguments});
    write_framed(&mut self.writer, &msg);
    self.seq
  }

  fn until(&mut self, mut pred: impl FnMut(&Value) -> bool) -> Value {
    loop {
      let Some(m) = read_framed(&mut self.reader) else {
        panic!("adapter closed the connection; seen: {:#?}", self.seen);
      };
      self.seen.push(m.to_string().chars().take(200).collect());
      if pred(&m) {
        return m;
      }
    }
  }

  fn response(&mut self, seq: i64) -> Value {
    let r = self.until(|m| m["type"] == "response" && m["request_seq"] == seq);
    assert_eq!(r["success"], true, "{r}");
    r
  }

  fn stopped(&mut self) -> Value {
    self.until(|m| m["event"] == "stopped" || m["event"] == "terminated")
  }

  fn breakpoint(&mut self) -> Option<Value> {
    loop {
      let event = self.stopped();
      if event["event"] == "terminated" {
        return None;
      }
      if event["body"]["reason"] == "breakpoint" {
        return Some(event);
      }
      let seq = self.send("continue", json!({"threadId": event["body"]["threadId"]}));
      self.response(seq);
    }
  }
}

/// Builds the launch exactly as `get_dap_binary` does, runs it, and debugs to a breakpoint.
fn debug_to_breakpoint(file: &str, source: &str, line: u32, expect_bound: bool) {
  let Some(elide) = elide() else {
    eprintln!("ELIDE_BIN not set; skipping");
    return;
  };
  let dir = fixture_dir(file);
  std::fs::write(dir.join(file), source).unwrap();
  let raw = json!({"request": "launch", "program": file}).to_string();
  let config = dap::Config::parse(&raw).unwrap();
  let port = free_port();
  let tcp = TcpArguments {
    port,
    host: u32::from(Ipv4Addr::LOCALHOST),
    timeout: None,
  };
  let bin = dap::binary(
    &config,
    raw.clone(),
    dap::Request::Launch,
    Some(elide),
    tcp,
    dir.display().to_string(),
  )
  .unwrap();
  assert_eq!(bin.request_args.request, StartDebuggingRequestArgumentsRequest::Launch);
  let child = Command::new(bin.command.unwrap())
    .args(&bin.arguments)
    .current_dir(bin.cwd.unwrap())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
  let mut child = Killer(child);
  let mut dap = Dap::connect(port, &mut child.0);

  let s = dap.send(
    "initialize",
    json!({"clientID": "zed", "adapterID": dap::ADAPTER, "linesStartAt1": true, "columnsStartAt1": true, "pathFormat": "path"}),
  );
  dap.response(s);
  let s = dap.send("launch", serde_json::from_str(&raw).unwrap());
  dap.response(s);
  let path = dir.join(file).display().to_string();
  let s = dap.send(
    "setBreakpoints",
    json!({"source": {"path": path, "name": file}, "breakpoints": [{"line": line}]}),
  );
  dap.response(s);
  let s = dap.send("configurationDone", json!({}));
  dap.response(s);

  // Entry-stop defaults differ between Elide versions; the requested breakpoint is the contract.
  let next = dap.breakpoint();
  if expect_bound {
    assert!(next.is_some(), "program terminated without reaching its breakpoint");
  }
  if let Some(next) = next {
    let s = dap.send("stackTrace", json!({"threadId": next["body"]["threadId"]}));
    let frames = dap.response(s);
    let top = &frames["body"]["stackFrames"][0];
    assert_eq!(top["line"], line, "{top}");
    assert!(top["source"]["path"].as_str().unwrap().ends_with(file), "{top}");
    eprintln!("{file}: verified breakpoint at line {line}");
  }
  // Elide may close the socket instead of answering a terminating disconnect.
  dap.send("disconnect", json!({"terminateDebuggee": true}));
  let _ = std::fs::remove_dir_all(&dir);
}

const JS: &str = "function add(a, b) {\n  const s = a + b;\n  return s;\n}\nconsole.log(add(1, 2));\n";
const TS: &str =
  "function add(a: number, b: number): number {\n  const s = a + b;\n  return s;\n}\nconsole.log(add(1, 2));\n";
const PY: &str = "def add(a, b):\n    s = a + b\n    return s\n\nprint(add(1, 2))\n";

#[test]
fn dap_launch_javascript() {
  debug_to_breakpoint("app.js", JS, 2, true);
}

#[test]
fn dap_launch_python() {
  if std::env::var("ELIDE_E2E_PYTHON").as_deref() != Ok("1") {
    eprintln!("ELIDE_E2E_PYTHON not set; skipping");
    return;
  }
  debug_to_breakpoint("app.py", PY, 2, true);
}

/// TypeScript breakpoints do not bind in Elide 1.6.0; this only checks that the session runs.
#[test]
fn dap_launch_typescript() {
  debug_to_breakpoint(
    "app.ts",
    TS,
    2,
    std::env::var("ELIDE_E2E_TS_BREAKPOINTS").as_deref() == Ok("1"),
  );
}

#[test]
fn locator_task_runs_under_debugger() {
  let Some(elide) = elide() else {
    eprintln!("ELIDE_BIN not set; skipping");
    return;
  };
  let dir = fixture_dir("locator");
  std::fs::write(dir.join("app.js"), JS).unwrap();
  let task = TaskTemplate {
    label: "run app.js".into(),
    command: elide.clone(),
    args: vec!["run".into(), "app.js".into()],
    env: vec![],
    cwd: Some(dir.display().to_string()),
  };
  let scenario = locator::create_scenario(&task, "run app.js").unwrap();
  let config = dap::Config::parse(&scenario.config).unwrap();
  let port = free_port();
  let args = dap::launch_args(&config, Ipv4Addr::LOCALHOST, port).unwrap();
  let child = Command::new(&elide)
    .args(&args)
    .current_dir(&dir)
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
  let mut child = Killer(child);
  let mut dap = Dap::connect(port, &mut child.0);
  let s = dap.send("initialize", json!({"clientID": "zed", "adapterID": dap::ADAPTER}));
  dap.response(s);
  let s = dap.send("launch", serde_json::from_str(&scenario.config).unwrap());
  dap.response(s);
  let s = dap.send(
    "setBreakpoints",
    json!({
      "source": {"path": dir.join("app.js").display().to_string()}, "breakpoints": [{"line": 2}]
    }),
  );
  dap.response(s);
  let s = dap.send("configurationDone", json!({}));
  dap.response(s);
  assert!(
    dap.breakpoint().is_some(),
    "task program did not stop at its breakpoint"
  );
  let _ = std::fs::remove_dir_all(&dir);
}

/// Attach mode: start `elide run --debugger=dap` separately and connect with an `attach` request.
#[test]
fn dap_attach_running_program() {
  let Some(elide) = elide() else {
    eprintln!("ELIDE_BIN not set; skipping");
    return;
  };
  let dir = fixture_dir("attach");
  std::fs::write(dir.join("app.js"), JS).unwrap();
  let port = free_port();
  let child = Command::new(&elide)
    .args(["run", "--debugger=dap", &format!("--debugger-port={port}"), "app.js"])
    .current_dir(&dir)
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
  let mut child = Killer(child);
  let raw = json!({"request": "attach", "port": port}).to_string();
  let config = dap::Config::parse(&raw).unwrap();
  let template = dap::tcp_template(&config, dap::Request::Attach, None).unwrap();
  assert_eq!(template.port, Some(port));
  let tcp = TcpArguments {
    port: template.port.unwrap(),
    host: template.host.unwrap_or(u32::from(Ipv4Addr::LOCALHOST)),
    timeout: None,
  };
  let bin = dap::binary(&config, raw.clone(), dap::Request::Attach, None, tcp, String::new()).unwrap();
  assert!(bin.command.is_none());
  let mut dap = Dap::connect(bin.connection.unwrap().port, &mut child.0);
  let s = dap.send("initialize", json!({"clientID": "zed", "adapterID": dap::ADAPTER}));
  dap.response(s);
  let s = dap.send("attach", serde_json::from_str(&raw).unwrap());
  dap.response(s);
  let s = dap.send(
    "setBreakpoints",
    json!({
      "source": {"path": dir.join("app.js").display().to_string()}, "breakpoints": [{"line": 2}]
    }),
  );
  dap.response(s);
  let s = dap.send("configurationDone", json!({}));
  dap.response(s);
  assert!(
    dap.breakpoint().is_some(),
    "attached program did not stop at its breakpoint"
  );
  let _ = std::fs::remove_dir_all(&dir);
}

fn lsp_roundtrip(elide: &str, root: &Path) -> (Value, Value) {
  let cmd = lsp::command(None, Some(elide.to_string()), vec![]).unwrap();
  let mut child = Killer(
    Command::new(&cmd.command)
      .args(&cmd.args)
      .current_dir(root)
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      .spawn()
      .unwrap(),
  );
  let mut stdin = child.0.stdin.take().unwrap();
  let mut stdout = BufReader::new(child.0.stdout.take().unwrap());
  let uri = format!("file://{}", root.display());
  write_framed(
    &mut stdin,
    &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"processId": null, "rootUri": uri, "capabilities": {}}}),
  );
  let mut init = Value::Null;
  while init.is_null() {
    let m = read_framed(&mut stdout).expect("lsp closed");
    if m["id"] == 1 {
      init = m;
    }
  }
  write_framed(
    &mut stdin,
    &json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
  );
  let file = format!("{uri}/Hello.java");
  write_framed(
    &mut stdin,
    &json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {
      "uri": file, "languageId": "java", "version": 1, "text": std::fs::read_to_string(root.join("Hello.java")).unwrap()}}}),
  );
  write_framed(
    &mut stdin,
    &json!({"jsonrpc": "2.0", "id": 2, "method": "textDocument/hover", "params": {
      "textDocument": {"uri": file}, "position": {"line": 3, "character": 24}}}),
  );
  let hover = loop {
    let m = read_framed(&mut stdout).expect("lsp closed");
    if m.get("method").is_some() && m.get("id").is_some() {
      write_framed(&mut stdin, &json!({"jsonrpc": "2.0", "id": m["id"], "result": null}));
    }
    if m["id"] == 2 && m.get("method").is_none() {
      break m;
    }
  };
  (init, hover)
}

#[test]
fn lsp_java_hover() {
  let Some(elide) = elide() else {
    eprintln!("ELIDE_BIN not set; skipping");
    return;
  };
  let dir = fixture_dir("lsp");
  std::fs::write(
    dir.join("elide.pkl"),
    "amends \"elide:project.pkl\"\n\nname = \"e2e\"\n",
  )
  .unwrap();
  std::fs::write(
    dir.join("Hello.java"),
    "public class Hello {\n  public static void main(String[] args) {\n    String greeting = \"hi\";\n    System.out.println(greeting.length());\n  }\n}\n",
  )
  .unwrap();
  let (init, hover) = lsp_roundtrip(&elide, &dir);
  assert_eq!(init["result"]["capabilities"]["hoverProvider"], true, "{init}");
  assert!(hover["result"]["contents"].to_string().contains("String"), "{hover}");
  let _ = std::fs::remove_dir_all(&dir);
}
