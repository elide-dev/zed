use std::{collections::BTreeMap, net::Ipv4Addr};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zed_extension_api::{
  DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario, StartDebuggingRequestArguments,
  StartDebuggingRequestArgumentsRequest, TcpArguments, TcpArgumentsTemplate,
};

pub const ADAPTER: &str = "Elide";
/// Elide's default `--debugger=dap` port.
pub const DEFAULT_PORT: u16 = 4711;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Request {
  Launch,
  Attach,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Subcommand {
  #[default]
  Run,
  Test,
}

impl Subcommand {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Run => "run",
      Self::Test => "test",
    }
  }
}

/// The `Elide` adapter's `debug.json` configuration; see `debug_adapter_schemas/Elide.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
  pub request: Option<Request>,
  pub program: Option<String>,
  #[serde(default)]
  pub args: Vec<String>,
  pub cwd: Option<String>,
  #[serde(default)]
  pub env: BTreeMap<String, String>,
  #[serde(default)]
  pub command: Subcommand,
  #[serde(default)]
  pub elide_args: Vec<String>,
  pub host: Option<String>,
  pub port: Option<u16>,
}

impl Config {
  pub fn parse(json: &str) -> Result<Self, String> {
    serde_json::from_str(json).map_err(|e| format!("invalid Elide debug configuration: {e}"))
  }
}

pub fn request_kind(config: &Value) -> Result<StartDebuggingRequestArgumentsRequest, String> {
  match config.get("request").and_then(Value::as_str) {
    Some("launch") => Ok(StartDebuggingRequestArgumentsRequest::Launch),
    Some("attach") => Ok(StartDebuggingRequestArgumentsRequest::Attach),
    Some(other) => Err(format!("unknown request `{other}`; expected `launch` or `attach`")),
    None => Err("`request` must be `launch` or `attach`".into()),
  }
}

/// TCP template to resolve on the host: `tcp_connection` wins, then `host`/`port`. A launch with no port gets a
/// free one; an attach defaults to Elide's port.
pub fn tcp_template(
  config: &Config,
  request: Request,
  user: Option<TcpArgumentsTemplate>,
) -> Result<TcpArgumentsTemplate, String> {
  let host = match &config.host {
    Some(h) if h == "localhost" => Some(u32::from(Ipv4Addr::LOCALHOST)),
    Some(h) => Some(u32::from(
      h.parse::<Ipv4Addr>()
        .map_err(|_| format!("`host` must be an IPv4 address, got `{h}`"))?,
    )),
    None => None,
  };
  let default_port = (request == Request::Attach).then_some(DEFAULT_PORT);
  let user = user.unwrap_or(TcpArgumentsTemplate {
    port: None,
    host: None,
    timeout: None,
  });
  Ok(TcpArgumentsTemplate {
    port: user.port.or(config.port).or(default_port),
    host: user.host.or(host),
    timeout: user.timeout,
  })
}

/// Arguments for `elide`: `<run|test> --debugger=dap --debugger-host=H --debugger-port=P [elideArgs] [program] [args]`.
pub fn launch_args(config: &Config, host: Ipv4Addr, port: u16) -> Result<Vec<String>, String> {
  if config.command == Subcommand::Run && config.program.is_none() && config.args.is_empty() {
    return Err("a launch needs `program` (or `args`)".into());
  }
  let mut out = vec![
    config.command.as_str().to_string(),
    "--debugger=dap".into(),
    format!("--debugger-host={host}"),
    format!("--debugger-port={port}"),
  ];
  out.extend(config.elide_args.iter().cloned());
  out.extend(config.program.iter().cloned());
  out.extend(config.args.iter().cloned());
  Ok(out)
}

/// Launch spawns `elide` and connects to its DAP listener; attach only connects.
pub fn binary(
  config: &Config,
  raw_config: String,
  request: Request,
  elide: Option<String>,
  tcp: TcpArguments,
  worktree_root: String,
) -> Result<DebugAdapterBinary, String> {
  let host = Ipv4Addr::from(tcp.host);
  let request_args = StartDebuggingRequestArguments {
    configuration: raw_config,
    request: match request {
      Request::Launch => StartDebuggingRequestArgumentsRequest::Launch,
      Request::Attach => StartDebuggingRequestArgumentsRequest::Attach,
    },
  };
  Ok(match request {
    Request::Launch => DebugAdapterBinary {
      command: Some(elide.ok_or_else(|| crate::NOT_FOUND.to_string())?),
      arguments: launch_args(config, host, tcp.port)?,
      envs: config.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
      cwd: Some(config.cwd.clone().unwrap_or(worktree_root)),
      connection: Some(tcp),
      request_args,
    },
    Request::Attach => DebugAdapterBinary {
      command: None,
      arguments: vec![],
      envs: vec![],
      cwd: None,
      connection: Some(tcp),
      request_args,
    },
  })
}

/// Turns the new-process modal's generic config into an Elide scenario.
pub fn scenario(config: DebugConfig) -> Result<DebugScenario, String> {
  let json = match config.request {
    DebugRequest::Launch(l) => {
      let mut c = json!({ "request": "launch", "program": l.program, "args": l.args });
      if let Some(cwd) = l.cwd {
        c["cwd"] = cwd.into();
      }
      if !l.envs.is_empty() {
        c["env"] = l
          .envs
          .into_iter()
          .map(|(k, v)| (k, v.into()))
          .collect::<serde_json::Map<_, _>>()
          .into();
      }
      c
    }
    DebugRequest::Attach(a) => {
      if a.process_id.is_some() {
        return Err("Elide attaches over TCP, not by process id; set `tcp_connection` or `port`".into());
      }
      json!({ "request": "attach", "port": DEFAULT_PORT })
    }
  };
  Ok(DebugScenario {
    label: config.label,
    adapter: ADAPTER.into(),
    build: None,
    config: json.to_string(),
    tcp_connection: None,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use zed_extension_api::{AttachRequest, LaunchRequest};

  fn tcp(port: u16) -> TcpArguments {
    TcpArguments {
      port,
      host: u32::from(Ipv4Addr::LOCALHOST),
      timeout: None,
    }
  }

  #[test]
  fn request_kind_requires_known_value() {
    assert_eq!(
      request_kind(&json!({"request": "launch"})).unwrap(),
      StartDebuggingRequestArgumentsRequest::Launch
    );
    assert_eq!(
      request_kind(&json!({"request": "attach"})).unwrap(),
      StartDebuggingRequestArgumentsRequest::Attach
    );
    assert!(request_kind(&json!({"request": "spawn"})).is_err());
    assert!(request_kind(&json!({})).is_err());
  }

  #[test]
  fn parse_full_config() {
    let c = Config::parse(
      r#"{"request":"launch","program":"app.ts","args":["a"],"cwd":"/w","env":{"K":"V"},
          "command":"test","elideArgs":["--verbose"],"port":5000,"stopOnEntry":true}"#,
    )
    .unwrap();
    assert_eq!(c.request, Some(Request::Launch));
    assert_eq!(c.command, Subcommand::Test);
    assert_eq!(c.elide_args, vec!["--verbose"]);
    assert_eq!(c.port, Some(5000));
  }

  #[test]
  fn launch_args_order() {
    let c = Config {
      program: Some("app.ts".into()),
      args: vec!["x".into(), "--y".into()],
      elide_args: vec!["--max-mem=512m".into()],
      ..Default::default()
    };
    assert_eq!(
      launch_args(&c, Ipv4Addr::LOCALHOST, 4800).unwrap(),
      vec![
        "run",
        "--debugger=dap",
        "--debugger-host=127.0.0.1",
        "--debugger-port=4800",
        "--max-mem=512m",
        "app.ts",
        "x",
        "--y"
      ]
    );
  }

  #[test]
  fn launch_test_without_program() {
    let c = Config {
      command: Subcommand::Test,
      ..Default::default()
    };
    assert_eq!(
      launch_args(&c, Ipv4Addr::LOCALHOST, 1).unwrap(),
      vec![
        "test",
        "--debugger=dap",
        "--debugger-host=127.0.0.1",
        "--debugger-port=1"
      ]
    );
    assert!(launch_args(&Config::default(), Ipv4Addr::LOCALHOST, 1).is_err());
  }

  #[test]
  fn tcp_template_defaults() {
    let launch = tcp_template(&Config::default(), Request::Launch, None).unwrap();
    assert_eq!((launch.port, launch.host), (None, None));
    let attach = tcp_template(&Config::default(), Request::Attach, None).unwrap();
    assert_eq!(attach.port, Some(DEFAULT_PORT));
  }

  #[test]
  fn tcp_template_precedence() {
    let c = Config {
      host: Some("10.0.0.2".into()),
      port: Some(6000),
      ..Default::default()
    };
    let t = tcp_template(&c, Request::Attach, None).unwrap();
    assert_eq!(t.port, Some(6000));
    assert_eq!(t.host, Some(u32::from(Ipv4Addr::new(10, 0, 0, 2))));
    let user = TcpArgumentsTemplate {
      port: Some(7000),
      host: None,
      timeout: Some(9),
    };
    let t = tcp_template(&c, Request::Attach, Some(user)).unwrap();
    assert_eq!((t.port, t.timeout), (Some(7000), Some(9)));
    assert_eq!(t.host, Some(u32::from(Ipv4Addr::new(10, 0, 0, 2))));
    let bad = Config {
      host: Some("::1".into()),
      ..Default::default()
    };
    assert!(tcp_template(&bad, Request::Attach, None).is_err());
    let local = Config {
      host: Some("localhost".into()),
      ..Default::default()
    };
    assert_eq!(
      tcp_template(&local, Request::Launch, None).unwrap().host,
      Some(u32::from(Ipv4Addr::LOCALHOST))
    );
  }

  #[test]
  fn launch_binary_spawns_elide() {
    let raw = r#"{"request":"launch","program":"app.py","env":{"K":"V"}}"#;
    let c = Config::parse(raw).unwrap();
    let b = binary(
      &c,
      raw.into(),
      Request::Launch,
      Some("/bin/elide".into()),
      tcp(4900),
      "/w".into(),
    )
    .unwrap();
    assert_eq!(b.command.as_deref(), Some("/bin/elide"));
    assert_eq!(b.arguments.last().map(String::as_str), Some("app.py"));
    assert!(b.arguments.contains(&"--debugger-port=4900".to_string()));
    assert_eq!(b.cwd.as_deref(), Some("/w"));
    assert_eq!(b.envs, vec![("K".to_string(), "V".to_string())]);
    assert_eq!(b.connection.unwrap().port, 4900);
    assert_eq!(b.request_args.request, StartDebuggingRequestArgumentsRequest::Launch);
    assert_eq!(b.request_args.configuration, raw);
  }

  #[test]
  fn launch_binary_needs_elide() {
    let c = Config {
      program: Some("a.js".into()),
      ..Default::default()
    };
    assert!(binary(&c, "{}".into(), Request::Launch, None, tcp(1), "/w".into()).is_err());
  }

  #[test]
  fn attach_binary_only_connects() {
    let b = binary(
      &Config::default(),
      "{}".into(),
      Request::Attach,
      None,
      tcp(4711),
      "/w".into(),
    )
    .unwrap();
    assert!(b.command.is_none());
    assert_eq!(b.connection.unwrap().port, 4711);
    assert_eq!(b.request_args.request, StartDebuggingRequestArgumentsRequest::Attach);
  }

  #[test]
  fn scenario_from_launch() {
    let s = scenario(DebugConfig {
      label: "app".into(),
      adapter: ADAPTER.into(),
      request: DebugRequest::Launch(LaunchRequest {
        program: "app.ts".into(),
        cwd: Some("/w".into()),
        args: vec!["1".into()],
        envs: vec![("K".into(), "V".into())],
      }),
      stop_on_entry: None,
    })
    .unwrap();
    assert_eq!(s.adapter, ADAPTER);
    let c: Value = serde_json::from_str(&s.config).unwrap();
    assert_eq!(
      c,
      json!({"request":"launch","program":"app.ts","args":["1"],"cwd":"/w","env":{"K":"V"}})
    );
    Config::parse(&s.config).unwrap();
  }

  #[test]
  fn scenario_from_attach() {
    let base = |pid| DebugConfig {
      label: "a".into(),
      adapter: ADAPTER.into(),
      request: DebugRequest::Attach(AttachRequest { process_id: pid }),
      stop_on_entry: None,
    };
    let s = scenario(base(None)).unwrap();
    assert_eq!(
      serde_json::from_str::<Value>(&s.config).unwrap(),
      json!({"request":"attach","port":4711})
    );
    assert!(scenario(base(Some(42))).is_err());
  }
}
