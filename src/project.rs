//! Pure conversion of Elide's version-1 IntelliJ export to Komodo project.properties.
//!
//! The host supplies exports and classpath replies; this module never reads the filesystem.
//! Directory selectors and recursive whole-source globs are representable. Selective or
//! wildcard-directory globs are rejected instead of indexing unrelated source directories.

use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

/// One `elide classpath <query> --json -p <project_root>` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClasspathQuery {
  /// Original exported module name, used to apply the reply.
  pub module: String,
  pub project_root: String,
  pub query: String,
  pub provided_query: String,
}

#[derive(Debug)]
pub struct Project {
  modules: Vec<Module>,
  /// Explicitly reported omissions, never unknown dependency targets or invalid paths.
  pub skipped: Vec<String>,
}

#[derive(Debug)]
struct Module {
  name: String,
  query: ClasspathQuery,
  sources: Vec<String>,
  dependencies: Vec<usize>,
  friends: Vec<usize>,
  classpath: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceExport {
  format: String,
  version: u32,
  root: String,
  projects: Vec<ProjectExport>,
  modules: Vec<ModuleExport>,
}

#[derive(Deserialize)]
struct ProjectExport {
  name: String,
  path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModuleExport {
  name: String,
  project: String,
  source_set: String,
  #[serde(rename = "type")]
  kind: String,
  test: bool,
  source_roots: Vec<SourceRoot>,
  module_dependencies: Vec<ModuleDependency>,
}

#[derive(Deserialize)]
struct SourceRoot {
  path: String,
  test: bool,
  #[serde(default)]
  generated: bool,
}

#[derive(Deserialize)]
struct ModuleDependency {
  module: String,
  scope: String,
}

impl Project {
  /// Parse the authoritative workspace graph. All paths in the export are based at
  /// `root`, including member source roots (not at each member's project directory).
  pub fn parse(json: &str) -> Result<Self, String> {
    Self::from_export(json, false)
  }

  /// The JVM debugger also needs source roots for Java-only modules.
  pub fn for_debugger(json: &str) -> Result<Self, String> {
    Self::from_export(json, true)
  }

  fn from_export(json: &str, include_java: bool) -> Result<Self, String> {
    let export: WorkspaceExport =
      serde_json::from_str(json).map_err(|error| format!("Invalid Elide IntelliJ workspace export: {error}"))?;
    if export.format != "elide.intellij.workspace" || export.version != 1 {
      return Err("Expected elide.intellij.workspace version 1".into());
    }
    let root = absolute_path(&export.root, None)?;
    let mut projects = HashMap::new();
    let mut project_roots = HashSet::new();
    for project in &export.projects {
      nonempty(&project.name, "project name")?;
      let member_root = absolute_path(&project.path, Some(&root))?;
      if !under(&member_root, &root) {
        return Err(format!("Project {} is outside workspace {root}", project.name));
      }
      if projects.insert(project.name.as_str(), member_root.clone()).is_some() || !project_roots.insert(member_root) {
        return Err(format!("Duplicate project name or root: {}", project.name));
      }
    }
    if projects.is_empty() || export.modules.is_empty() {
      return Err("Workspace has no usable source modules".into());
    }
    let mut indices = HashMap::new();
    let mut source_sets = HashSet::new();
    let mut names = HashSet::new();
    let mut modules = Vec::with_capacity(export.modules.len());
    for (index, module) in export.modules.iter().enumerate() {
      nonempty(&module.name, "module name")?;
      nonempty(&module.source_set, "source set")?;
      if module.source_set.starts_with('-')
        || module.source_set.contains([':', ',', '/', '\\'])
        || module.source_set.chars().any(char::is_whitespace)
      {
        return Err(format!("Unqueryable source set {}", module.source_set));
      }
      let project_root = projects
        .get(module.project.as_str())
        .ok_or_else(|| format!("Unknown project {} for {}", module.project, module.name))?;
      if indices.insert(module.name.as_str(), index).is_some()
        || !source_sets.insert((&module.project, &module.source_set))
      {
        return Err(format!("Duplicate module or project source set: {}", module.name));
      }
      if !matches!(module.kind.as_str(), "source" | "test" | "example") || module.test != (module.kind != "source") {
        return Err(format!("Unsupported or inconsistent source type for {}", module.name));
      }
      let mut sources = Vec::new();
      for source in &module.source_roots {
        if source.test != module.test {
          return Err(format!("Inconsistent source root type for {}", module.name));
        }
        // Generated roots are literal directories in the export, with the same base.
        if source.generated && source.path.contains(['*', '?', '[', '{']) {
          return Err(format!("Generated source root is not literal: {}", source.path));
        }
        // Without filesystem access, only explicit Java selectors prove that
        // a root has no Kotlin. Literal directories remain authoritative.
        if !include_java && source.path.ends_with(".java") {
          continue;
        }
        let path = source_path(&source.path, &root).map_err(|error| format!("{}: {error}", module.name))?;
        push_unique(&mut sources, path);
      }
      let base: String = module
        .name
        .chars()
        .map(|character| {
          if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
            character
          } else {
            '_'
          }
        })
        .collect();
      let base = if base.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        base
      } else {
        format!("_{base}")
      };
      let mut name = base.clone();
      let mut suffix = 2;
      while !names.insert(name.clone()) {
        name = format!("{base}_{suffix}");
        suffix += 1;
      }
      modules.push(Module {
        name,
        query: ClasspathQuery {
          module: module.name.clone(),
          project_root: project_root.clone(),
          query: format!("{}:compile", module.source_set),
          provided_query: format!("{}:provided", module.source_set),
        },
        sources,
        dependencies: Vec::new(),
        friends: Vec::new(),
        classpath: None,
      });
    }
    for (index, module) in export.modules.iter().enumerate() {
      for dependency in &module.module_dependencies {
        scope(&dependency.scope)?;
        let target = *indices
          .get(dependency.module.as_str())
          .ok_or_else(|| format!("{} references missing module {}", module.name, dependency.module))?;
        push_unique(&mut modules[index].dependencies, target);
      }
      if module.test {
        // Elide's test compile query contains test-scoped jars only. Like the
        // reference model, tests see their own project's production source sets.
        for (target, candidate) in export.modules.iter().enumerate() {
          if candidate.project == module.project && !candidate.test {
            push_unique(&mut modules[index].dependencies, target);
            push_unique(&mut modules[index].friends, target);
          }
        }
      }
    }
    let mut skipped = Vec::new();
    let mut retained = 0;
    let remap: Vec<_> = modules
      .iter()
      .map(|module| {
        if module.sources.is_empty() {
          skipped.push(format!("{}: no Kotlin source roots", module.query.module));
          None
        } else {
          let index = retained;
          retained += 1;
          Some(index)
        }
      })
      .collect();
    if retained == 0 {
      return Err("Workspace has no Kotlin source roots for Komodo".into());
    }
    for module in &mut modules {
      if module.sources.is_empty() {
        continue;
      }
      module.dependencies.retain_mut(|target| {
        if let Some(index) = remap[*target] {
          *target = index;
          true
        } else {
          skipped.push(format!(
            "{}: dependency {} has no Kotlin source roots; its compiled classes must be on the resolved classpath",
            module.query.module, export.modules[*target].name
          ));
          false
        }
      });
      module.friends.retain_mut(|target| {
        if let Some(index) = remap[*target] {
          *target = index;
          true
        } else {
          false
        }
      });
    }
    modules.retain(|module| !module.sources.is_empty());
    let project = Self { modules, skipped };
    project.dependency_order()?;
    Ok(project)
  }

  pub fn classpath_queries(&self) -> Vec<ClasspathQuery> {
    self.modules.iter().map(|module| module.query.clone()).collect()
  }

  /// Apply resolved entries atomically: a malformed path must not replace a valid classpath.
  pub fn apply_classpath(&mut self, module: &str, entries: &[String]) -> Result<(), String> {
    let target = self
      .modules
      .iter_mut()
      .find(|candidate| candidate.query.module == module)
      .ok_or_else(|| format!("Unknown module {module}"))?;
    let mut classpath = Vec::with_capacity(entries.len());
    for entry in entries {
      nonempty(entry, "classpath entry")?;
      push_unique(&mut classpath, absolute_path(entry, Some(&target.query.project_root))?);
    }
    target.classpath = Some(classpath);
    Ok(())
  }

  /// Render only after every query has succeeded. Uses absolute paths because the
  /// manifest lives in the extension sandbox, not the workspace. Does not install,
  /// build, discover Kotlin files, or infer compiler plugins/toolchain settings.
  pub fn render(&self) -> Result<String, String> {
    for module in &self.modules {
      if module.classpath.is_none() {
        return Err(format!("Missing resolved classpath for {}", module.query.module));
      }
    }
    let names: Vec<_> = self.modules.iter().map(|module| module.name.as_str()).collect();
    let mut output = format!(
      "# Generated by the Elide Zed extension; refreshed on server restart.\nmodules={}\ncache=off\n",
      names.join(",")
    );
    for module in &self.modules {
      write_property(&mut output, &module.name, "sources", &module.sources);
      for (key, edges) in [("depends", &module.dependencies), ("friends", &module.friends)] {
        if !edges.is_empty() {
          let values: Vec<_> = edges.iter().map(|&target| self.modules[target].name.as_str()).collect();
          write_property(&mut output, &module.name, key, &values);
        }
      }
      let mut classpath: Vec<&str> = module.classpath.as_ref().unwrap().iter().map(String::as_str).collect();
      for &friend in &module.friends {
        for path in self.modules[friend].classpath.as_ref().unwrap() {
          push_unique(&mut classpath, path.as_str());
        }
      }
      if !classpath.is_empty() {
        write_property(&mut output, &module.name, "classpath", &classpath);
      }
    }
    Ok(output)
  }

  fn dependency_order(&self) -> Result<Vec<usize>, String> {
    let mut remaining: Vec<_> = self.modules.iter().map(|module| module.dependencies.len()).collect();
    let mut dependents = vec![Vec::new(); self.modules.len()];
    for (index, module) in self.modules.iter().enumerate() {
      for &dependency in &module.dependencies {
        dependents[dependency].push(index);
      }
    }
    let mut order: Vec<_> = remaining
      .iter()
      .enumerate()
      .filter_map(|(index, &count)| (count == 0).then_some(index))
      .collect();
    let mut next = 0;
    while next < order.len() {
      for &dependent in &dependents[order[next]] {
        remaining[dependent] -= 1;
        if remaining[dependent] == 0 {
          order.push(dependent);
        }
      }
      next += 1;
    }
    if order.len() != self.modules.len() {
      return Err("Workspace module dependency graph contains a cycle".into());
    }
    Ok(order)
  }
}

fn nonempty(value: &str, what: &str) -> Result<(), String> {
  if value.trim().is_empty() || value.chars().any(char::is_control) {
    Err(format!("Invalid {what}: {value:?}"))
  } else {
    Ok(())
  }
}

fn scope(value: &str) -> Result<(), String> {
  if matches!(value, "COMPILE" | "TEST" | "PROVIDED" | "RUNTIME") {
    Ok(())
  } else {
    Err(format!("Unknown dependency scope {value:?}"))
  }
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
  if !values.contains(&value) {
    values.push(value);
  }
}

fn under(path: &str, root: &str) -> bool {
  path == root
    || path
      .strip_prefix(root)
      .is_some_and(|suffix| root.ends_with('/') || suffix.starts_with('/'))
}

fn source_path(selector: &str, root: &str) -> Result<String, String> {
  nonempty(selector, "source selector")?;
  let Some(wildcard) = selector.find(['*', '?', '[', '{']) else {
    return absolute_path(selector, Some(root));
  };
  let directory_end = selector[..wildcard].rfind('/').map_or(0, |index| index + 1);
  let pattern = &selector[directory_end..];
  // Whole-source recursive selectors preserve directory boundaries. Java selectors
  // reach this function only when constructing a JVM debugger model.
  if !matches!(
    pattern,
    "**" | "**/*" | "**/*.kt" | "**/*.java" | "**/*.{kt,java}" | "**/*.{java,kt}"
  ) {
    return Err(format!(
      "Source selector {selector:?} cannot be represented without broadening its sources; configure an explicit Komodo project"
    ));
  }
  let directory = if directory_end == 0 {
    "."
  } else {
    &selector[..directory_end]
  };
  absolute_path(directory, Some(root))
}

/// Lexical path resolution, independent of the native or WASI host's path syntax.
/// Drive-absolute Windows paths are supported; UNC and ambiguous backslashes are not.
fn absolute_path(value: &str, base: Option<&str>) -> Result<String, String> {
  if value.contains(',') || value.chars().any(char::is_control) || value.trim() != value {
    return Err(format!("Path cannot be represented in a Komodo list: {value:?}"));
  }
  let drive = value.as_bytes().get(1) == Some(&b':') && value.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
  if value.starts_with("//") || value.starts_with("\\\\") {
    return Err(format!("UNC paths are not supported: {value:?}"));
  }
  if drive && !value.as_bytes().get(2).is_some_and(|byte| matches!(byte, b'/' | b'\\')) {
    return Err(format!("Drive-relative path is not supported: {value:?}"));
  }
  let windows = drive || base.is_some_and(|root| root.as_bytes().get(1) == Some(&b':'));
  if value.contains('\\') && !windows {
    return Err(format!("Ambiguous backslash in path: {value:?}"));
  }
  let value = if windows {
    value.replace('\\', "/")
  } else {
    value.to_owned()
  };
  let absolute = if drive || value.starts_with('/') {
    value
  } else if let Some(base) = base {
    format!("{base}/{value}")
  } else {
    return Err(format!("Workspace root must be absolute: {value:?}"));
  };
  let (prefix, rest) = if absolute.as_bytes().get(1) == Some(&b':') {
    (&absolute[..3], &absolute[3..])
  } else {
    ("/", &absolute[1..])
  };
  let mut parts = Vec::new();
  for component in rest.split('/') {
    match component {
      "" | "." => {}
      ".." => {
        if parts.pop().is_none() {
          return Err(format!("Path traverses above its filesystem root: {absolute:?}"));
        }
      }
      component => parts.push(component),
    }
  }
  let result = format!("{prefix}{}", parts.join("/"));
  if result.trim() != result {
    return Err(format!("Path cannot be represented in a Komodo list: {result:?}"));
  }
  Ok(result)
}

fn write_property(output: &mut String, module: &str, key: &str, values: &[impl AsRef<str>]) {
  // Paths have no controls/backslashes and never start with whitespace. Properties
  // only treats #, !, = and : specially before the value, so they need no escaping.
  write!(output, "module.{module}.{key}=").expect("writing a String cannot fail");
  for (index, value) in values.iter().enumerate() {
    if index > 0 {
      output.push(',');
    }
    output.push_str(value.as_ref());
  }
  output.push('\n');
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::{Value, json};

  fn module(name: &str, project: &str, source_set: &str, sources: &[&str], test: bool) -> Value {
    json!({
        "name": name, "project": project, "sourceSet": source_set,
        "type": if test { "test" } else { "source" }, "test": test,
        "contentRoots": ["unrelated-ancestor"],
        "sourceRoots": sources.iter().map(|path| json!({"path":path,"test":test})).collect::<Vec<_>>(),
        "resourceRoots": [], "moduleDependencies": [], "libraryDependencies": []
    })
  }

  fn workspace() -> Value {
    json!({
        "format": "elide.intellij.workspace", "version": 1, "name": "workspace",
        "root": "/work", "localRepository": "/work/.dev/repository",
        "projects": [{"name":"app","path":"apps/app"}, {"name":"lib","path":"libs/lib"}],
        "modules": [
            module("app.main", "app", "main", &["apps/app/src/main/kotlin/**/*.kt", "apps/app/generated"], false),
            module("app.test", "app", "test", &["apps/app/src/test/kotlin"], true),
            module("lib.main", "lib", "main", &["libs/lib/src"], false)
        ], "libraries": []
    })
  }

  fn resolved(value: &Value) -> Project {
    let mut project = Project::parse(&value.to_string()).unwrap();
    for query in project.classpath_queries() {
      project.apply_classpath(&query.module, &[]).unwrap();
    }
    project
  }

  #[test]
  fn member_roots_edges_friends_and_test_inheritance() {
    let mut value = workspace();
    value["modules"][0]["moduleDependencies"] = json!([{"module":"lib.main","scope":"COMPILE"}]);
    let mut project = resolved(&value);
    let queries = project.classpath_queries();
    assert_eq!(
      queries[0],
      ClasspathQuery {
        module: "app.main".into(),
        project_root: "/work/apps/app".into(),
        query: "main:compile".into(),
        provided_query: "main:provided".into(),
      }
    );
    project
      .apply_classpath(
        "app.main",
        &[
          "../../jars/base.jar".into(),
          "/sdk/kotlin.jar".into(),
          "/sdk/kotlin.jar".into(),
        ],
      )
      .unwrap();
    project.apply_classpath("app.test", &["/sdk/test.jar".into()]).unwrap();
    let text = project.render().unwrap();
    assert!(text.contains("module.app_main.sources=/work/apps/app/src/main/kotlin,/work/apps/app/generated\n"));
    assert!(text.contains("module.app_main.depends=lib_main\n"));
    assert!(text.contains("module.app_test.depends=app_main\n"));
    assert!(text.contains("module.app_test.friends=app_main\n"));
    assert!(text.contains("module.app_test.classpath=/sdk/test.jar,/work/jars/base.jar,/sdk/kotlin.jar\n"));
    assert!(!text.contains("unrelated-ancestor"));
    assert!(!text.contains("friends=lib_main"));
  }

  #[test]
  fn source_selectors_never_expand_to_unrelated_directories() {
    for (selector, expected) in [
      ("src/main/**/*.kt", "/work/src/main"),
      ("src/main/**", "/work/src/main"),
      ("src/main/**/*", "/work/src/main"),
      ("./src/../generated/", "/work/generated"),
      ("Main.kt", "/work/Main.kt"),
      ("src/**/*.{kt,java}", "/work/src"),
      ("**/*.kt", "/work"),
    ] {
      assert_eq!(source_path(selector, "/work").unwrap(), expected);
    }
    for selector in [
      "src/m*/**/*.kt",
      "src/*/kotlin",
      "src/?.kt",
      "src/[ab]/**",
      "src/{a,b}/**",
      "src/Only*.kt",
      "src/*.kt",
    ] {
      assert!(source_path(selector, "/work").is_err(), "{selector}");
    }
    assert_eq!(source_path("src-other/**/*.kt", "/work").unwrap(), "/work/src-other");
  }

  #[test]
  fn malformed_or_unusable_models_fail() {
    for field in ["projects", "modules"] {
      let mut value = workspace();
      value.as_object_mut().unwrap().remove(field);
      assert!(Project::parse(&value.to_string()).is_err(), "{field}");
    }
    for (field, replacement) in [
      ("version", json!(2)),
      ("format", json!("other")),
      ("root", json!("relative")),
      ("modules", json!([])),
    ] {
      let mut value = workspace();
      value[field] = replacement;
      assert!(Project::parse(&value.to_string()).is_err(), "{field}");
    }
    for (field, replacement) in [
      ("project", json!("missing")),
      ("sourceRoots", json!(null)),
      ("type", json!("other")),
      ("test", json!(true)),
      ("sourceSet", json!("main:runtime")),
      ("moduleDependencies", json!([{"module":"missing","scope":"COMPILE"}])),
    ] {
      let mut value = workspace();
      value["modules"][0][field] = replacement;
      assert!(Project::parse(&value.to_string()).is_err(), "{field}");
    }
    assert!(Project::parse("not json").is_err());
  }

  #[test]
  fn java_only_modules_are_reported_without_losing_kotlin_graph() {
    let mut value = workspace();
    value["modules"][0]["moduleDependencies"] = json!([{"module":"lib.main","scope":"COMPILE"}]);
    value["modules"][2]["sourceRoots"] = json!([{"path":"libs/lib/src/**/*.java","test":false}]);
    let project = resolved(&value);
    assert_eq!(project.classpath_queries().len(), 2);
    assert!(
      project
        .skipped
        .iter()
        .any(|reason| reason.contains("lib.main: no Kotlin"))
    );
    assert!(
      project
        .skipped
        .iter()
        .any(|reason| reason.contains("app.main: dependency lib.main"))
    );
    let text = project.render().unwrap();
    assert!(text.contains("modules=app_main,app_test\n"));
    assert!(text.contains("module.app_test.friends=app_main\n"));
    assert!(!text.contains("lib_main"));
    let mut debugger = Project::for_debugger(&value.to_string()).unwrap();
    for query in debugger.classpath_queries() {
      debugger.apply_classpath(&query.module, &[]).unwrap();
    }
    let debug_text = debugger.render().unwrap();
    assert!(debug_text.contains("module.app_main.depends=lib_main\n"));
    assert!(debug_text.contains("module.lib_main.sources=/work/libs/lib/src\n"));
    value["modules"][2]["moduleDependencies"] = json!([{"module":"unknown","scope":"COMPILE"}]);
    assert!(Project::parse(&value.to_string()).is_err());
    for module in value["modules"].as_array_mut().unwrap() {
      module["sourceRoots"] = json!([]);
      module["moduleDependencies"] = json!([]);
    }
    assert!(Project::parse(&value.to_string()).is_err());
  }

  #[test]
  fn duplicates_and_cycles_are_rejected() {
    let mut value = workspace();
    value["modules"][2]["name"] = json!("app.main");
    assert!(Project::parse(&value.to_string()).is_err());
    let mut value = workspace();
    value["modules"][0]["moduleDependencies"] = json!([{"module":"app.test","scope":"COMPILE"}]);
    assert!(Project::parse(&value.to_string()).unwrap_err().contains("cycle"));
    let mut value = workspace();
    value["projects"][1]["path"] = json!("apps/app");
    assert!(Project::parse(&value.to_string()).is_err());
    value["projects"][1]["path"] = json!("../work-other");
    assert!(Project::parse(&value.to_string()).is_err());
  }

  #[test]
  fn sanitized_names_remain_unique_and_edges_use_them() {
    let mut value = workspace();
    value["modules"][0]["name"] = json!("1.app");
    value["modules"][1]["name"] = json!("1_app");
    value["modules"][2]["name"] = json!("1_app_2");
    let text = resolved(&value).render().unwrap();
    assert!(text.contains("modules=_1_app,_1_app_2,_1_app_2_2\n"));
    assert!(text.contains("module._1_app_2.friends=_1_app\n"));
  }

  #[test]
  fn classpaths_are_required_and_updates_are_atomic() {
    let mut project = Project::parse(&workspace().to_string()).unwrap();
    assert!(project.render().is_err());
    assert!(project.apply_classpath("app.main", &["".into()]).is_err());
    project.apply_classpath("app.main", &["/valid.jar".into()]).unwrap();
    assert!(
      project
        .apply_classpath("app.main", &["/replacement.jar".into(), "/bad,jar".into()])
        .is_err()
    );
    assert_eq!(project.modules[0].classpath.as_ref().unwrap(), &["/valid.jar"]);
    assert!(project.apply_classpath("absent", &[]).is_err());
  }

  #[test]
  fn properties_list_boundaries_and_path_normalization() {
    for path in [
      "/has,comma",
      "/has\nnewline",
      "/has\rreturn",
      "/has\ttab",
      " /leading",
      "/trailing ",
      "/bad\0path",
      "/literal\\backslash",
    ] {
      assert!(absolute_path(path, Some("/work")).is_err(), "{path:?}");
    }
    assert!(absolute_path("../../../escape", Some("/work")).is_err());
    assert!(absolute_path("C:relative", None).is_err());
    assert!(absolute_path("//server/share", None).is_err());
    assert_eq!(absolute_path("C:\\work\\src\\..\\lib", None).unwrap(), "C:/work/lib");
    assert_eq!(
      absolute_path("jars\\dep.jar", Some("C:/work")).unwrap(),
      "C:/work/jars/dep.jar"
    );
    assert_eq!(absolute_path("/", None).unwrap(), "/");
    let mut value = workspace();
    value["modules"][0]["sourceRoots"][0]["path"] = json!("a #!:= café/Main.kt");
    assert!(
      resolved(&value)
        .render()
        .unwrap()
        .contains("sources=/work/a #!:= café/Main.kt,/work/apps/app/generated\n")
    );
    value["modules"][0]["sourceRoots"][0]["path"] = json!("bad,path");
    assert!(Project::parse(&value.to_string()).is_err());
  }
}
