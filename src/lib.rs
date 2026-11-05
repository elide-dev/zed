mod dap;
#[cfg(test)]
mod e2e;
mod komodo;
mod locator;
mod lsp;
mod project;

use std::{collections::BTreeMap, path::PathBuf};

use zed_extension_api::{
  self as zed, DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario, DebugTaskDefinition, LanguageServerId,
  StartDebuggingRequestArgumentsRequest, TaskTemplate, Worktree, resolve_tcp_template,
  serde_json::Value,
  settings::{CommandSettings, LspSettings},
};

pub(crate) const NOT_FOUND: &str =
  "`elide` was not found on PATH; install it from https://elide.dev or set its path in settings";

struct ElideExtension;

const KOMODO_SERVER: &str = "elide-komodo";

struct KomodoLaunch {
  resolution: komodo::Resolution,
  settings: komodo::Settings,
  binary: Option<CommandSettings>,
  manifest: String,
  env: Vec<(String, String)>,
  windows: bool,
}

fn cli_output(executable: &str, args: Vec<String>, env: &[(String, String)]) -> zed::Result<String> {
  let output = zed::process::Command::new(executable)
    .args(args)
    .envs(env.iter().cloned())
    .output()?;
  if output.status != Some(0) {
    return Err(format!(
      "Elide command failed (exit {:?}): {}\n{}",
      output.status,
      String::from_utf8_lossy(&output.stderr),
      String::from_utf8_lossy(&output.stdout),
    ));
  }
  String::from_utf8(output.stdout).map_err(|e| format!("Elide returned non-UTF-8 output: {e}"))
}

fn komodo_launch(worktree: &Worktree, adapter_path: Option<String>, debugger: bool) -> zed::Result<KomodoLaunch> {
  let lsp = LspSettings::for_worktree(KOMODO_SERVER, worktree)?;
  let windows = zed::current_platform().0 == zed::Os::Windows;
  let settings = komodo::Settings::parse(lsp.settings.as_ref(), windows)?;
  let env = worktree.shell_env();
  let elide = settings
    .elide_path
    .clone()
    .or_else(|| {
      LspSettings::for_worktree("elide", worktree)
        .ok()
        .and_then(|s| s.binary)
        .and_then(|b| b.path)
    })
    .or_else(|| worktree.which("elide"));
  let explicit = adapter_path.or_else(|| settings.komodo_path.clone());
  let resolution = match explicit {
    Some(path) => komodo::Resolution::explicit(&path, windows)?,
    None => {
      let executable = elide.as_deref().ok_or(NOT_FOUND)?;
      let json = cli_output(executable, vec!["komodo".into(), "resolve".into(), "--json".into()], &env)
        .map_err(|e| format!("{e}\nUse an Elide version with managed Komodo, or set lsp.elide-komodo.settings.komodoPath. No installation was attempted."))?;
      komodo::Resolution::managed(&json, windows)?
    }
  };
  let manifest = match settings.project.as_ref() {
    Some(path) => path.clone(),
    None => {
      let executable = elide.as_deref().ok_or(NOT_FOUND)?;
      let json = cli_output(
        executable,
        vec![
          "project".into(),
          "export".into(),
          "--format=intellij".into(),
          "-p".into(),
          worktree.root_path(),
        ],
        &env,
      )?;
      let mut project = if debugger {
        project::Project::for_debugger(&json)?
      } else {
        project::Project::parse(&json)?
      };
      for reason in &project.skipped {
        eprintln!("Elide Komodo: {reason}");
      }
      let mut groups = BTreeMap::<String, Vec<project::ClasspathQuery>>::new();
      for query in project.classpath_queries() {
        groups.entry(query.project_root.clone()).or_default().push(query);
      }
      for (root, queries) in groups {
        let mut args = vec!["classpath".into()];
        args.extend(
          queries
            .iter()
            .flat_map(|query| [query.query.clone(), query.provided_query.clone()]),
        );
        args.extend(["--json".into(), "-p".into(), root]);
        let json = cli_output(executable, args, &env)?;
        let mut replies: BTreeMap<String, Vec<String>> =
          serde_json::from_str(&json).map_err(|e| format!("Invalid Elide classpath JSON: {e}"))?;
        for query in queries {
          let mut entries = replies
            .remove(&query.query)
            .ok_or_else(|| format!("Elide did not answer classpath query {}", query.query))?;
          let provided = replies
            .remove(&query.provided_query)
            .ok_or_else(|| format!("Elide did not answer classpath query {}", query.provided_query))?;
          entries.extend(provided);
          project.apply_classpath(&query.module, &entries)?;
        }
      }
      let content = project.render()?;
      // WASI can write extension storage, not the user's workspace. Each worktree owns its manifest.
      let dir = PathBuf::from("komodo-projects");
      std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
      let mode = if debugger { "dap" } else { "lsp" };
      let path = dir.join(format!("{}-{mode}.properties", worktree.id()));
      let temporary = path.with_extension("tmp");
      std::fs::write(&temporary, content).map_err(|e| e.to_string())?;
      std::fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
      std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join(path)
        .to_string_lossy()
        .into_owned()
    }
  };
  Ok(KomodoLaunch {
    resolution,
    settings,
    binary: lsp.binary,
    manifest,
    env,
    windows,
  })
}

impl zed::Extension for ElideExtension {
  fn new() -> Self {
    Self
  }

  fn language_server_command(&mut self, id: &LanguageServerId, worktree: &Worktree) -> zed::Result<zed::Command> {
    match id.as_ref() {
      "elide" => {
        let settings = LspSettings::for_worktree(id.as_ref(), worktree)?;
        lsp::command(settings.binary.as_ref(), worktree.which("elide"), worktree.shell_env())
      }
      KOMODO_SERVER => {
        let launch = komodo_launch(worktree, None, false)?;
        komodo::command(
          &launch.resolution,
          &launch.settings,
          launch.binary.as_ref(),
          &launch.manifest,
          launch.env,
          launch.windows,
        )
      }
      other => Err(format!("unknown language server `{other}`")),
    }
  }

  fn language_server_initialization_options(
    &mut self,
    id: &LanguageServerId,
    worktree: &Worktree,
  ) -> zed::Result<Option<Value>> {
    Ok(
      LspSettings::for_worktree(id.as_ref(), worktree)
        .ok()
        .and_then(|s| s.initialization_options),
    )
  }

  fn language_server_workspace_configuration(
    &mut self,
    id: &LanguageServerId,
    worktree: &Worktree,
  ) -> zed::Result<Option<Value>> {
    if id.as_ref() == KOMODO_SERVER {
      // These settings configure the launcher, not the LSP workspace.
      return Ok(None);
    }
    Ok(
      LspSettings::for_worktree(id.as_ref(), worktree)
        .ok()
        .and_then(|s| s.settings),
    )
  }

  fn get_dap_binary(
    &mut self,
    adapter_name: String,
    task: DebugTaskDefinition,
    user_provided_debug_adapter_path: Option<String>,
    worktree: &Worktree,
  ) -> Result<DebugAdapterBinary, String> {
    if adapter_name == komodo::ADAPTER {
      if task.tcp_connection.is_some() {
        return Err("Komodo uses stdio; set `port` in the attach configuration for JDWP, not `tcp_connection`".into());
      }
      let launch = komodo_launch(worktree, user_provided_debug_adapter_path, true)?;
      let command = komodo::debug_command(
        &launch.resolution,
        &launch.settings,
        launch.binary.as_ref(),
        &launch.manifest,
        launch.env,
        launch.windows,
      )?;
      return komodo::debug_binary(command, task.config, worktree.root_path());
    }
    if adapter_name != dap::ADAPTER {
      return Err(format!("unknown debug adapter `{adapter_name}`"));
    }
    let raw: Value = serde_json::from_str(&task.config).map_err(|e| e.to_string())?;
    let request = match dap::request_kind(&raw)? {
      StartDebuggingRequestArgumentsRequest::Launch => dap::Request::Launch,
      StartDebuggingRequestArgumentsRequest::Attach => dap::Request::Attach,
    };
    let config = dap::Config::parse(&task.config)?;
    let tcp = resolve_tcp_template(dap::tcp_template(&config, request, task.tcp_connection)?)?;
    let elide = user_provided_debug_adapter_path.or_else(|| worktree.which("elide"));
    dap::binary(&config, task.config, request, elide, tcp, worktree.root_path())
  }

  fn dap_request_kind(
    &mut self,
    adapter_name: String,
    config: Value,
  ) -> Result<StartDebuggingRequestArgumentsRequest, String> {
    match adapter_name.as_str() {
      dap::ADAPTER => dap::request_kind(&config),
      komodo::ADAPTER => komodo::request_kind(&config),
      other => Err(format!("unknown debug adapter `{other}`")),
    }
  }

  fn dap_config_to_scenario(&mut self, config: DebugConfig) -> Result<DebugScenario, String> {
    match config.adapter.as_str() {
      dap::ADAPTER => dap::scenario(config),
      komodo::ADAPTER => komodo::scenario(config),
      other => Err(format!("unknown debug adapter `{other}`")),
    }
  }

  fn dap_locator_create_scenario(
    &mut self,
    locator_name: String,
    build_task: TaskTemplate,
    resolved_label: String,
    _debug_adapter_name: String,
  ) -> Option<DebugScenario> {
    (locator_name == locator::NAME)
      .then(|| locator::create_scenario(&build_task, &resolved_label))
      .flatten()
  }

  fn run_dap_locator(&mut self, _locator_name: String, _build_task: TaskTemplate) -> Result<DebugRequest, String> {
    Err("the Elide locator resolves scenarios without a build step".into())
  }
}

zed::register_extension!(ElideExtension);
