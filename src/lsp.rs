use zed_extension_api::{Command, settings::CommandSettings};

pub const DEFAULT_ARGS: &[&str] = &["lsp"];

/// Builds the `elide lsp` command. An explicit `lsp.elide.binary.path` wins over `PATH`; explicit `arguments`
/// replace the default `lsp`; settings `env` overrides the login shell environment.
pub fn command(
  binary: Option<&CommandSettings>,
  which: Option<String>,
  shell_env: Vec<(String, String)>,
) -> Result<Command, String> {
  let command = binary
    .and_then(|b| b.path.clone())
    .or(which)
    .ok_or_else(|| crate::NOT_FOUND.to_string())?;
  let args = binary
    .and_then(|b| b.arguments.clone())
    .unwrap_or_else(|| DEFAULT_ARGS.iter().map(|s| s.to_string()).collect());
  let mut env = shell_env;
  if let Some(extra) = binary.and_then(|b| b.env.as_ref()) {
    for (k, v) in extra {
      env.retain(|(ek, _)| ek != k);
      env.push((k.clone(), v.clone()));
    }
  }
  Ok(Command { command, args, env })
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::HashMap;

  fn settings(path: Option<&str>, args: Option<&[&str]>, env: Option<&[(&str, &str)]>) -> CommandSettings {
    CommandSettings {
      path: path.map(Into::into),
      arguments: args.map(|a| a.iter().map(|s| s.to_string()).collect()),
      env: env.map(|e| {
        e.iter()
          .map(|(k, v)| (k.to_string(), v.to_string()))
          .collect::<HashMap<_, _>>()
      }),
    }
  }

  #[test]
  fn defaults_to_path_lookup() {
    let Command {
      command: p,
      args: a,
      env: e,
    } = command(None, Some("/usr/bin/elide".into()), vec![("A".into(), "1".into())]).unwrap();
    assert_eq!(p, "/usr/bin/elide");
    assert_eq!(a, vec!["lsp"]);
    assert_eq!(e, vec![("A".to_string(), "1".to_string())]);
  }

  #[test]
  fn missing_binary_errors() {
    let err = command(None, None, vec![]).unwrap_err();
    assert!(err.contains("elide"), "{err}");
  }

  #[test]
  fn settings_override_path_args_env() {
    let s = settings(
      Some("/opt/elide/bin/elide"),
      Some(&["lsp", "--verbose"]),
      Some(&[("A", "2"), ("B", "3")]),
    );
    let Command {
      command: p,
      args: a,
      env: e,
    } = command(Some(&s), Some("/usr/bin/elide".into()), vec![("A".into(), "1".into())]).unwrap();
    assert_eq!(p, "/opt/elide/bin/elide");
    assert_eq!(a, vec!["lsp", "--verbose"]);
    assert_eq!(e.iter().filter(|(k, _)| k == "A").count(), 1);
    assert!(e.contains(&("A".into(), "2".into())));
    assert!(e.contains(&("B".into(), "3".into())));
  }

  #[test]
  fn arguments_only_keeps_path_lookup() {
    let s = settings(None, Some(&["--debug", "lsp"]), None);
    let Command {
      command: p, args: a, ..
    } = command(Some(&s), Some("/usr/bin/elide".into()), vec![]).unwrap();
    assert_eq!(p, "/usr/bin/elide");
    assert_eq!(a, vec!["--debug", "lsp"]);
  }
}
