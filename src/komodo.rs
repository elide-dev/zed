use serde::Deserialize;
use serde_json::Value;
use zed_extension_api::{
  Command, DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario, StartDebuggingRequestArguments,
  StartDebuggingRequestArgumentsRequest, settings::CommandSettings,
};

pub const ADAPTER: &str = "Komodo";

/// Check the host's syntax, not WASI's Unix-only `Path::is_absolute` implementation.
pub fn is_absolute(path: &str, windows: bool) -> bool {
  if path.is_empty() || path.contains('\0') {
    return false;
  }
  if !windows {
    return path.starts_with('/');
  }
  let bytes = path.as_bytes();
  if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\') {
    return true;
  }
  if path.starts_with("\\\\") || path.starts_with("//") {
    let mut components = path[2..].split(['/', '\\']);
    return components.next().is_some_and(|s| !s.is_empty()) && components.next().is_some_and(|s| !s.is_empty());
  }
  false
}

fn absolute(path: &str, field: &str, windows: bool) -> Result<(), String> {
  if is_absolute(path, windows) {
    Ok(())
  } else {
    Err(format!("Komodo `{field}` must be an absolute host path, got `{path}`"))
  }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
  pub project: Option<String>,
  pub komodo_path: Option<String>,
  pub elide_path: Option<String>,
  pub java_home: Option<String>,
  pub max_heap: Option<String>,
  #[serde(default)]
  pub plugins: Vec<String>,
}

impl Settings {
  pub fn parse(value: Option<&Value>, windows: bool) -> Result<Self, String> {
    let settings: Self = match value {
      None | Some(Value::Null) => Self::default(),
      Some(value) => {
        serde_json::from_value(value.clone()).map_err(|e| format!("invalid lsp.elide-komodo settings: {e}"))?
      }
    };
    for (field, path) in [
      ("project", settings.project.as_deref()),
      ("komodoPath", settings.komodo_path.as_deref()),
      ("elidePath", settings.elide_path.as_deref()),
      ("javaHome", settings.java_home.as_deref()),
    ] {
      if let Some(path) = path {
        absolute(path, field, windows)?;
      }
    }
    if let Some(heap) = &settings.max_heap {
      let digits = heap.strip_suffix(['k', 'K', 'm', 'M', 'g', 'G']).unwrap_or(heap);
      if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) || !digits.bytes().any(|b| b != b'0') {
        return Err("Komodo `maxHeap` must be a positive size such as `768m` or `1g`".into());
      }
    }
    for plugin in &settings.plugins {
      let (id, path) = plugin
        .split_once('=')
        .ok_or("Komodo `plugins` entries must be `id=/absolute/plugin.jar`")?;
      if id.is_empty() || id.chars().any(char::is_whitespace) {
        return Err("Komodo compiler plugin IDs must be nonempty and contain no whitespace".into());
      }
      absolute(path, "plugins", windows)?;
    }
    Ok(settings)
  }
}

/// Shared launcher identity for language analysis and debugging. The host's process launch
/// reports missing executables and permission errors; WASI cannot inspect arbitrary host files.
#[derive(Debug, Clone)]
pub struct Resolution {
  pub executable: String,
  pub java_home: Option<String>,
}

impl Resolution {
  /// An override is authoritative; an invalid override must never trigger managed resolution.
  pub fn explicit(path: &str, windows: bool) -> Result<Self, String> {
    absolute(
      path,
      "binary.path (fix or clear this override; no managed fallback)",
      windows,
    )?;
    Ok(Self {
      executable: path.into(),
      java_home: None,
    })
  }

  /// Parse successful stdout from `elide komodo resolve --json`. Never installs a distribution.
  pub fn managed(json: &str, windows: bool) -> Result<Self, String> {
    let data: Value = serde_json::from_str(json).map_err(|e| format!(
      "Invalid Komodo resolution JSON: {e}. Upgrade Elide to support `elide komodo resolve --json`, or configure binary.path."
    ))?;
    if data.get("schemaVersion").and_then(Value::as_u64) != Some(1)
      || data.get("component").and_then(Value::as_str) != Some("komodo")
    {
      return Err("Unsupported Komodo resolution schema; upgrade Elide and the extension together".into());
    }
    let status = data.get("status").and_then(Value::as_str);
    if status != Some("resolved") {
      return match (status, data.get("message").and_then(Value::as_str)) {
        (Some(status @ ("missing" | "unsupported" | "incompatible" | "error")), Some(message)) => {
          Err(format!("Komodo {status}: {message}"))
        }
        _ => Err("Elide returned an invalid Komodo resolution failure".into()),
      };
    }
    let protocol = &data["protocol"];
    let capabilities = data.get("capabilities").and_then(Value::as_array);
    if !data
      .get("version")
      .and_then(Value::as_str)
      .is_some_and(|v| !v.is_empty())
      || protocol.get("lsp").and_then(Value::as_u64) != Some(1)
      || protocol.get("dap").and_then(Value::as_u64) != Some(1)
      || !capabilities.is_some_and(|values| {
        values.iter().all(Value::is_string)
          && ["lsp", "dap"]
            .iter()
            .all(|required| values.iter().any(|v| v.as_str() == Some(required)))
      })
    {
      return Err("Installed Komodo must provide protocol lsp=1 and dap=1 and capabilities lsp/dap; install a compatible distribution through Elide".into());
    }
    let field = |name: &str| -> Result<&str, String> {
      let path = data
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Komodo identity is missing `{name}`"))?;
      absolute(path, name, windows)?;
      Ok(path)
    };
    field("bundle")?;
    field("pluginDirectory")?;
    Ok(Self {
      executable: field("executable")?.into(),
      java_home: Some(field("javaHome")?.into()),
    })
  }
}

pub fn command(
  resolution: &Resolution,
  settings: &Settings,
  binary: Option<&CommandSettings>,
  manifest: &str,
  shell_env: Vec<(String, String)>,
  windows: bool,
) -> Result<Command, String> {
  build_command(resolution, settings, binary, manifest, shell_env, windows, false)
}

pub fn debug_command(
  resolution: &Resolution,
  settings: &Settings,
  binary: Option<&CommandSettings>,
  manifest: &str,
  shell_env: Vec<(String, String)>,
  windows: bool,
) -> Result<Command, String> {
  build_command(resolution, settings, binary, manifest, shell_env, windows, true)
}

fn build_command(
  resolution: &Resolution,
  settings: &Settings,
  binary: Option<&CommandSettings>,
  manifest: &str,
  mut shell_env: Vec<(String, String)>,
  windows: bool,
  dap: bool,
) -> Result<Command, String> {
  absolute(manifest, "project", windows)?;
  let executable = &resolution.executable;
  let args = if let Some(custom) = binary.and_then(|b| b.arguments.as_ref()).filter(|_| !dap) {
    custom.clone()
  } else {
    let mut args = Vec::new();
    if let Some(heap) = &settings.max_heap {
      args.push(format!("-Xmx{heap}"));
    }
    if dap {
      args.push("--dap".into());
    }
    args.extend(["--project".into(), manifest.into()]);
    for plugin in &settings.plugins {
      args.extend(["--plugin".into(), plugin.clone()]);
    }
    args
  };
  let mut set_env = |key: &str, value: &str| {
    shell_env.retain(|(k, _)| {
      if windows {
        !k.eq_ignore_ascii_case(key)
      } else {
        k != key
      }
    });
    shell_env.push((key.into(), value.into()));
  };
  if let Some(home) = settings.java_home.as_ref().or(resolution.java_home.as_ref()) {
    set_env("JAVA_HOME", home);
  }
  if let Some(env) = binary.and_then(|b| b.env.as_ref()) {
    for (key, value) in env {
      set_env(key, value);
    }
  }
  Ok(Command {
    command: executable.into(),
    args,
    env: shell_env,
  })
}

pub fn request_kind(config: &Value) -> Result<StartDebuggingRequestArgumentsRequest, String> {
  match config.get("request").and_then(Value::as_str) {
    Some("launch") => Ok(StartDebuggingRequestArgumentsRequest::Launch),
    Some("attach") => Ok(StartDebuggingRequestArgumentsRequest::Attach),
    _ => Err("Komodo `request` must be `launch` or `attach`".into()),
  }
}

/// Validate known fields without reserializing: preserve all adapter-specific payload fields.
pub fn validate_debug(config: &Value) -> Result<StartDebuggingRequestArgumentsRequest, String> {
  let request = request_kind(config)?;
  for field in ["processId", "process_id", "pid"] {
    if config.get(field).is_some() {
      return Err("Komodo supports JDWP attach by hostName/port, not process ID".into());
    }
  }
  for field in ["mainClass", "cwd", "javaHome", "hostName"] {
    if config
      .get(field)
      .is_some_and(|value| !value.as_str().is_some_and(|s| !s.trim().is_empty()))
    {
      return Err(format!("Komodo `{field}` must be a nonempty string"));
    }
  }
  for field in ["classPaths", "vmArgs", "args", "sourceRoots"] {
    if config
      .get(field)
      .is_some_and(|value| !value.as_array().is_some_and(|a| a.iter().all(Value::is_string)))
    {
      return Err(format!("Komodo `{field}` must be an array of strings"));
    }
  }
  if config
    .get("env")
    .is_some_and(|value| !value.as_object().is_some_and(|o| o.values().all(Value::is_string)))
  {
    return Err("Komodo `env` must be an object of string values".into());
  }
  if config
    .get("port")
    .is_some_and(|value| !value.as_u64().is_some_and(|p| (1..=65535).contains(&p)))
  {
    return Err("Komodo `port` must be a JDWP port from 1 through 65535, not an adapter transport port".into());
  }
  if config.get("timeout").is_some_and(|value| value.as_u64().is_none()) {
    return Err("Komodo `timeout` must be a nonnegative integer in milliseconds".into());
  }
  match request {
    StartDebuggingRequestArgumentsRequest::Launch => {
      if config.get("mainClass").is_none() || config.get("classPaths").is_none() {
        return Err("Komodo JVM launch requires `mainClass` and `classPaths` in debug.json".into());
      }
    }
    StartDebuggingRequestArgumentsRequest::Attach => {
      if config.get("port").is_none() {
        return Err("Komodo JDWP attach requires `port` in debug.json".into());
      }
    }
  }
  Ok(request)
}

pub fn debug_binary(command: Command, raw_config: String, worktree_root: String) -> Result<DebugAdapterBinary, String> {
  let config: Value =
    serde_json::from_str(&raw_config).map_err(|e| format!("Invalid Komodo debug configuration: {e}"))?;
  let request = validate_debug(&config)?;
  Ok(DebugAdapterBinary {
    command: Some(command.command),
    arguments: command.args,
    envs: command.env,
    cwd: Some(worktree_root),
    connection: None,
    request_args: StartDebuggingRequestArguments {
      configuration: raw_config,
      request,
    },
  })
}

pub fn scenario(config: DebugConfig) -> Result<DebugScenario, String> {
  match config.request {
    DebugRequest::Launch(_) => Err("Komodo JVM launch needs `mainClass` and `classPaths`, which Zed's generic launch dialog cannot represent. Add a Komodo launch configuration to .zed/debug.json.".into()),
    DebugRequest::Attach(attach) if attach.process_id.is_some() =>
      Err("Komodo does not support PID attach. Add a Komodo attach configuration with the JVM's JDWP `hostName` and `port` to .zed/debug.json.".into()),
    DebugRequest::Attach(_) => Err("Komodo attach needs the JVM's JDWP `port`, not a TCP adapter connection. Add a Komodo attach configuration with `hostName` and `port` to .zed/debug.json.".into()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  fn managed() -> Value {
    json!({"schemaVersion":1,"component":"komodo","status":"resolved","version":"1.0",
      "protocol":{"lsp":1,"dap":1},"capabilities":["lsp","dap"],
      "executable":"/komodo/bin/komodo","bundle":"/komodo","pluginDirectory":"/komodo/plugins","javaHome":"/jdk"})
  }

  #[test]
  fn host_paths_do_not_use_wasi_rules() {
    assert!(is_absolute("/tmp/project.json", false));
    assert!(!is_absolute("C:\\project.json", false));
    assert!(is_absolute("C:\\project.json", true));
    assert!(is_absolute("C:/project.json", true));
    assert!(is_absolute("\\\\server\\share\\project.json", true));
    for path in ["C:relative", "relative", "/root-relative", "\\\\server", ""] {
      assert!(!is_absolute(path, true), "{path}");
    }
  }

  #[test]
  fn managed_identity_requires_both_services_and_all_paths() {
    assert!(Resolution::managed(&managed().to_string(), false).is_ok());
    for field in ["executable", "bundle", "pluginDirectory", "javaHome"] {
      let mut value = managed();
      value[field] = json!("relative");
      assert!(Resolution::managed(&value.to_string(), false).is_err());
    }
    for patch in [
      json!({"protocol":{"lsp":1,"dap":2}}),
      json!({"capabilities":["lsp"]}),
      json!({"version":""}),
      json!({"schemaVersion":2}),
      json!({"component":"other"}),
    ] {
      let mut value = managed();
      value
        .as_object_mut()
        .unwrap()
        .extend(patch.as_object().unwrap().clone());
      assert!(Resolution::managed(&value.to_string(), false).is_err());
    }
    let error = Resolution::managed(
      r#"{"schemaVersion":1,"component":"komodo","status":"missing","message":"Install Komodo explicitly"}"#,
      false,
    )
    .unwrap_err();
    assert!(error.contains("Install Komodo explicitly"));
  }

  #[test]
  fn settings_validate_host_paths_heap_and_plugins() {
    assert!(
      Settings::parse(
        Some(&json!({"project":"/project.json","maxHeap":"768m","plugins":["id=/plugin.jar"]})),
        false
      )
      .is_ok()
    );
    for value in [
      json!({"project":"relative"}),
      json!({"komodoPath":"relative"}),
      json!({"javaHome":2}),
      json!({"maxHeap":"0g"}),
      json!({"maxHeap":"1g --dap"}),
      json!({"plugins":["id=relative.jar"]}),
      json!({"plugins":["=/plugin.jar"]}),
    ] {
      assert!(Settings::parse(Some(&value), false).is_err());
    }
    assert!(Resolution::explicit("relative", false).is_err());
  }

  #[test]
  fn lsp_and_dap_share_identity_flags_and_environment() {
    // Resolution already incorporates the DAP-specific path override, ahead of the LSP setting.
    let resolution = Resolution::explicit("/debugger/komodo", false).unwrap();
    let settings = Settings::parse(
      Some(&json!({"maxHeap":"1g","plugins":["id=/plugin.jar"],"javaHome":"/custom-jdk"})),
      false,
    )
    .unwrap();
    let binary = CommandSettings {
      path: Some("/developer/komodo".into()),
      arguments: Some(vec!["--verbose".into()]),
      env: None,
    };
    let lsp = command(
      &resolution,
      &settings,
      Some(&binary),
      "/project.json",
      vec![("JAVA_HOME".into(), "/old".into())],
      false,
    )
    .unwrap();
    assert_eq!(lsp.command, "/debugger/komodo");
    assert_eq!(lsp.args, ["--verbose"]);
    let default_lsp = command(&resolution, &settings, None, "/project.json", vec![], false).unwrap();
    assert_eq!(
      default_lsp.args,
      ["-Xmx1g", "--project", "/project.json", "--plugin", "id=/plugin.jar"]
    );
    assert_eq!(lsp.env, vec![("JAVA_HOME".into(), "/custom-jdk".into())]);
    for raw in [
      r#"{ "request":"launch", "mainClass":"MainKt","classPaths":["/classes"],"args":["x"],"env":{"A":"B"},"custom":true }"#,
      r#"{"request":"attach","hostName":"localhost","port":5005,"timeout":30000,"sourceRoots":["/src"]}"#,
    ] {
      let command = debug_command(&resolution, &settings, Some(&binary), "/project.json", vec![], false).unwrap();
      let dap = debug_binary(command, raw.into(), "/workspace".into()).unwrap();
      assert_eq!(dap.command.as_deref(), Some(lsp.command.as_str()));
      assert_eq!(
        dap.arguments,
        [
          "-Xmx1g",
          "--dap",
          "--project",
          "/project.json",
          "--plugin",
          "id=/plugin.jar"
        ]
      );
      assert!(dap.connection.is_none());
      assert_eq!(dap.request_args.configuration, raw);
      assert_eq!(dap.envs, lsp.env);
    }
  }

  #[test]
  fn debug_validation_rejects_malformed_fields_without_losing_extensions() {
    for value in [
      json!({"request":"unknown"}),
      json!({"request":"launch"}),
      json!({"request":"launch","mainClass":"","classPaths":[]}),
      json!({"request":"launch","mainClass":"MainKt","classPaths":"/classes"}),
      json!({"request":"attach"}),
      json!({"request":"attach","port":0}),
      json!({"request":"attach","port":65536}),
      json!({"request":"attach","port":-1}),
      json!({"request":"attach","port":1.5}),
      json!({"request":"attach","port":5005,"processId":42}),
      json!({"request":"attach","port":5005,"timeout":-1}),
      json!({"request":"attach","port":5005,"env":{"A":false}}),
    ] {
      assert!(validate_debug(&value).is_err(), "{value}");
    }
    assert!(validate_debug(&json!({"request":"attach","port":65535,"custom":true})).is_ok());
  }
}
