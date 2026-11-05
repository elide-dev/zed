use serde_json::json;
use zed_extension_api::{DebugScenario, TaskTemplate};

use crate::dap::ADAPTER;

pub const NAME: &str = "elide";

fn is_elide(program: &str) -> bool {
  let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
  base == "elide" || base == "elide.exe"
}

/// Splits a shell command string into words, honoring quotes; `None` if it uses shell operators.
fn shell_words(command: &str) -> Option<Vec<String>> {
  let mut words = Vec::new();
  let mut cur: Option<String> = None;
  let mut chars = command.chars();
  while let Some(c) = chars.next() {
    match c {
      '\'' => {
        let w = cur.get_or_insert_with(String::new);
        loop {
          match chars.next()? {
            '\'' => break,
            ch => w.push(ch),
          }
        }
      }
      '"' => {
        let w = cur.get_or_insert_with(String::new);
        loop {
          match chars.next()? {
            '"' => break,
            '\\' => w.push(chars.next()?),
            ch => w.push(ch),
          }
        }
      }
      '\\' => cur.get_or_insert_with(String::new).push(chars.next()?),
      '|' | '&' | ';' | '<' | '>' | '`' | '(' | ')' => return None,
      c if c.is_whitespace() => words.extend(cur.take()),
      c => cur.get_or_insert_with(String::new).push(c),
    }
  }
  words.extend(cur);
  Some(words)
}

/// Drops `--debugger*` flags (and a separate value token) so the adapter's own listener flags are the only ones.
fn strip_debugger_flags(args: &[String]) -> Vec<String> {
  let mut out = Vec::with_capacity(args.len());
  let mut it = args.iter();
  while let Some(a) = it.next() {
    match a.as_str() {
      "--" => {
        out.push(a.clone());
        out.extend(it.cloned());
        break;
      }
      "--debugger-host" | "--debugger-port" => {
        it.next();
      }
      _ if a == "--debugger" || a.starts_with("--debugger=") || a.starts_with("--debugger-") => {}
      _ => out.push(a.clone()),
    }
  }
  out
}

/// Maps an `elide run …` or `elide test …` task to an Elide launch scenario, whatever adapter the language prefers.
pub fn create_scenario(task: &TaskTemplate, resolved_label: &str) -> Option<DebugScenario> {
  let (program, tokens): (String, Vec<String>) = if task.args.is_empty() {
    let mut words = shell_words(&task.command)?.into_iter();
    (words.next()?, words.collect())
  } else {
    (task.command.clone(), task.args.clone())
  };
  if !is_elide(&program) {
    return None;
  }
  let (sub, rest) = tokens.split_first()?;
  if sub != "run" && sub != "test" {
    return None;
  }
  let mut config = json!({
    "request": "launch",
    "command": sub,
    "args": strip_debugger_flags(rest),
  });
  if let Some(cwd) = &task.cwd {
    config["cwd"] = cwd.clone().into();
  }
  if !task.env.is_empty() {
    config["env"] = task
      .env
      .iter()
      .map(|(k, v)| (k.clone(), v.clone().into()))
      .collect::<serde_json::Map<_, _>>()
      .into();
  }
  Some(DebugScenario {
    label: resolved_label.to_string(),
    adapter: ADAPTER.into(),
    build: None,
    config: config.to_string(),
    tcp_connection: None,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::dap::{Config, Subcommand, launch_args};
  use serde_json::Value;
  use std::net::Ipv4Addr;

  fn task(command: &str, args: &[&str]) -> TaskTemplate {
    TaskTemplate {
      label: "t".into(),
      command: command.into(),
      args: args.iter().map(|s| s.to_string()).collect(),
      env: vec![],
      cwd: None,
    }
  }

  fn config(s: &DebugScenario) -> Value {
    serde_json::from_str(&s.config).unwrap()
  }

  #[test]
  fn run_task_becomes_launch() {
    let mut t = task("elide", &["run", "$ZED_FILE", "--flag"]);
    t.cwd = Some("$ZED_WORKTREE_ROOT".into());
    t.env = vec![("K".into(), "V".into())];
    let s = create_scenario(&t, "run app.ts").unwrap();
    assert_eq!(s.adapter, ADAPTER);
    assert_eq!(s.label, "run app.ts");
    assert!(s.build.is_none());
    assert_eq!(
      config(&s),
      json!({"request":"launch","command":"run","args":["$ZED_FILE","--flag"],
             "cwd":"$ZED_WORKTREE_ROOT","env":{"K":"V"}})
    );
    let c = Config::parse(&s.config).unwrap();
    assert_eq!(
      launch_args(&c, Ipv4Addr::LOCALHOST, 5).unwrap(),
      vec![
        "run",
        "--debugger=dap",
        "--debugger-host=127.0.0.1",
        "--debugger-port=5",
        "$ZED_FILE",
        "--flag"
      ]
    );
  }

  #[test]
  fn test_task_and_shell_string() {
    let s = create_scenario(&task("/opt/elide/bin/elide test sample.test.mts", &[]), "t").unwrap();
    let c = Config::parse(&s.config).unwrap();
    assert_eq!(c.command, Subcommand::Test);
    assert_eq!(c.args, vec!["sample.test.mts"]);
  }

  #[test]
  fn shell_words_quotes() {
    assert_eq!(
      shell_words(r#"elide run "$ZED_FILE" 'a b' c\ d"#).unwrap(),
      vec!["elide", "run", "$ZED_FILE", "a b", "c d"]
    );
    assert!(shell_words("elide run \"unterminated").is_none());
    assert!(shell_words("elide run a | tee").is_none());
  }

  #[test]
  fn strips_existing_debugger_flags() {
    let s = create_scenario(
      &task(
        "elide",
        &[
          "run",
          "--debugger=dap",
          "--debugger-port",
          "9",
          "--debugger-host=0.0.0.0",
          "app.js",
        ],
      ),
      "t",
    )
    .unwrap();
    assert_eq!(config(&s)["args"], json!(["app.js"]));
  }

  #[test]
  fn preserves_application_debugger_arguments() {
    let s = create_scenario(&task("elide", &["run", "app.js", "--", "--debugger-port", "9"]), "app").unwrap();
    assert_eq!(config(&s)["args"], json!(["app.js", "--", "--debugger-port", "9"]));
  }

  #[test]
  fn ignores_other_tasks() {
    assert!(create_scenario(&task("node", &["app.js"]), "t").is_none());
    assert!(create_scenario(&task("elide", &["build"]), "t").is_none());
    assert!(create_scenario(&task("elide", &[]), "t").is_none());
    assert!(create_scenario(&task("elide run a.js && echo done", &[]), "t").is_none());
    assert!(create_scenario(&task("elidex", &["run", "a.js"]), "t").is_none());
    assert!(create_scenario(&task("C:\\elide\\elide.exe", &["run", "a.js"]), "t").is_some());
  }
}
