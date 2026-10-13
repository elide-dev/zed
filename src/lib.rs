mod dap;
#[cfg(test)]
mod e2e;
mod locator;
mod lsp;

use zed_extension_api::{
  self as zed, DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario, DebugTaskDefinition, LanguageServerId,
  StartDebuggingRequestArgumentsRequest, TaskTemplate, Worktree, resolve_tcp_template, serde_json::Value,
  settings::LspSettings,
};

pub(crate) const NOT_FOUND: &str =
  "`elide` was not found on PATH; install it from https://elide.dev or set its path in settings";

struct ElideExtension;

impl zed::Extension for ElideExtension {
  fn new() -> Self {
    Self
  }

  fn language_server_command(&mut self, id: &LanguageServerId, worktree: &Worktree) -> zed::Result<zed::Command> {
    let settings = LspSettings::for_worktree(id.as_ref(), worktree).unwrap_or_default();
    lsp::command(settings.binary.as_ref(), worktree.which("elide"), worktree.shell_env())
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
    _adapter_name: String,
    config: Value,
  ) -> Result<StartDebuggingRequestArgumentsRequest, String> {
    dap::request_kind(&config)
  }

  fn dap_config_to_scenario(&mut self, config: DebugConfig) -> Result<DebugScenario, String> {
    dap::scenario(config)
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
