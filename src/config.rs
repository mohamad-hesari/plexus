use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(untagged)]
pub enum OptionConfig<T, N = bool> {
  None(N),
  List(Vec<T>),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Dependency {
  #[serde(skip_serializing_if = "Option::is_none", default)]
  pub name: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none", default)]
  pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigWatch {
  #[serde(skip_serializing_if = "Option::is_none", default)]
  pub path: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none", default)]
  pub include: Option<Vec<String>>,
  #[serde(skip_serializing_if = "Option::is_none", default)]
  pub exclude: Option<Vec<String>>,
  #[serde(skip_serializing_if = "Option::is_none", default)]
  pub disable_default_exclude: Option<bool>,
  #[serde(skip_serializing)]
  pub default_exclude: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlexusCommand {
  pub name: String,
  pub actual_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(untagged)]
pub enum ConfigCommand {
  Simple(String),
  Plexus(PlexusCommand),
  WithDependency {
    command: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    depends_on: Option<Vec<Dependency>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    envs: Option<ConfigEnvs>,
  },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigEnv {
  pub key: String,
  pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ConfigEnvs {
  None,
  List(Vec<ConfigEnv>),
  File(String),
  Files(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigProject {
  pub name: String,
  pub path: String,
  pub depends_on: OptionConfig<String>,
  #[serde(skip_serializing_if = "Option::is_none", default)]
  pub watches: Option<Vec<ConfigWatch>>,
  pub commands: Vec<ConfigCommand>,
  #[serde(skip_serializing_if = "Option::is_none", default)]
  pub envs: Option<ConfigEnv>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Config {
  #[serde(rename = "$schema")]
  pub schema: String,
  pub version: String,
  pub projects: Vec<ConfigProject>,
  #[serde(skip_serializing_if = "Option::is_none", default)]
  pub envs: Option<ConfigEnvs>,
}

fn read_json_file(path: &PathBuf) -> Option<serde_json::Value> {
  if path.exists() {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    serde_json::from_reader(reader).ok()
  } else {
    None
  }
}

pub fn read_package_json(path: &str) -> Option<serde_json::Value> {
  let package_json_path = Path::new(path).join("package.json");
  read_json_file(&package_json_path)
}

// fn extract_escaped_glob(base_str: &str, include_glob_str: &str) -> Option<(PathBuf, String)> {
//   let mut static_parts = Vec::new();
//   let mut glob_parts = Vec::new();
//   let mut found_wildcard = false;
//
//   for component in Path::new(include_glob_str).components() {
//     let comp_str = component.as_os_str().to_string_lossy();
//     if found_wildcard || comp_str.contains('*') || comp_str.contains('?') {
//       found_wildcard = true;
//       glob_parts.push(comp_str.into_owned());
//     } else {
//       static_parts.push(comp_str.into_owned());
//     }
//   }
//
//   let static_include_path: PathBuf = static_parts.iter().collect();
//   let glob_pattern = glob_parts.join("/");
//
//   // 2. Canonicalize the base path and the static part of the include path
//   let base_canonical = Path::new(base_str).canonicalize().ok()?;
//   let real_static_path = base_canonical.join(static_include_path).canonicalize().ok()?;
//
//   // 3. Check if it escaped.
//   // If it STILL starts with base, it did not escape (it's a sub-path).
//   if real_static_path.starts_with(&base_canonical) {
//     println!("Path is inside the base directory. No adjustments needed.");
//     return None;
//   }
//
//   // 4. It escaped! Return the absolute base directory and the clean glob suffix.
//   Some((real_static_path, glob_pattern))
// }
//
// fn read_watches_from_json(path: &str, value: &serde_json::Value) -> Option<Vec<ConfigWatch>> {
//   let include = value.get("include")?.as_array()?;
//   let exclude = value.get("exclude")?.as_array()?;
//   let base_path = Path::new(path).canonicalize().ok()?;
//   let mut watches = HashMap::new();
//   for include_path in include {
//     let include_path_str = include_path.as_str()?.to_string();
//     let real_path = base_path.join(include_path_str.clone()).canonicalize().ok()?;
//     if real_path.starts_with(&base_path) {
//       if !watches.contains_key(&base_path) {
//         watches.insert(
//           base_path.clone(),
//           ConfigWatch {
//             path: None,
//             include: Some(vec![]),
//             exclude: Some(vec![]),
//           },
//         );
//       }
//       watches
//         .get_mut(&base_path)?
//         .include
//         .as_mut()?
//         .push(include_path_str.clone());
//     } else {
//       let include_path = include_path_str.clone();
//       if let Some((adjusted_base, glob_pattern)) = extract_escaped_glob(path, &include_path.as_str()) {
//         if !watches.contains_key(&adjusted_base) {
//           watches.insert(
//             adjusted_base.clone(),
//             ConfigWatch {
//               path: Some(adjusted_base.to_str()?.to_string()),
//               include: Some(vec![]),
//               exclude: Some(vec![]),
//             },
//           );
//         }
//         watches.get_mut(&adjusted_base)?.include.as_mut()?.push(glob_pattern);
//       }
//     }
//   }
//   None
// }
//
// fn read_watches(path: &str) -> Option<Vec<ConfigWatch>> {
//   let tsconfig_json_path = Path::new(path).join("tsconfig.json");
//   let tsconfig_json = read_json_file(&tsconfig_json_path)?;
//   let include = tsconfig_json.get("include");
//   if include.is_none() {
//     let refrences = tsconfig_json.get("references")?.as_array()?;
//     if refrences.is_empty() {
//       return None;
//     }
//     let mut result = Vec::new();
//     for reference in refrences {
//       let ref_path = reference.get("path")?.as_str()?;
//       let ref_tsconfig_json_path = Path::new(path).join(ref_path);
//       let ref_tsconfig_json = read_json_file(&ref_tsconfig_json_path)?;
//       if let Some(watches) = read_watches_from_json(path, &ref_tsconfig_json) {
//         result.extend(watches);
//       }
//     }
//     Some(result)
//   } else {
//     read_watches_from_json(path, &tsconfig_json)
//   }
// }

pub async fn init_default_config() -> Config {
  let current_dir = std::env::current_dir().expect("Failed to get current directory");
  let root_package_json = read_package_json(current_dir.to_str().unwrap()).unwrap_or_else(|| {
    panic!(
      "Failed to read package.json from current directory: {}",
      current_dir.display()
    )
  });
  let root_project_name = root_package_json["name"].as_str().unwrap_or("").to_string();
  let cmd = Command::new("pnpm")
    .arg("list")
    .arg("--recursive")
    .arg("--depth=-1")
    .arg("--json")
    .output()
    .await
    .expect("Failed to execute pnpm list command");
  let stdout = String::from_utf8_lossy(&cmd.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout).expect("Failed to parse pnpm list output");
  let mut projects = Vec::new();
  let Some(project_json) = json.as_array() else {
    panic!("Unexpected pnpm list output format, expected an array of projects");
  };
  for project in project_json {
    let name = project["name"].as_str().unwrap_or("").to_string();
    let path = project["path"].as_str().unwrap_or("").to_string();
    if name == root_project_name {
      continue;
    }
    let normalized_path = Path::new(&path)
      .strip_prefix(&current_dir)
      .unwrap_or(Path::new(&path))
      .to_path_buf();
    let project_package_json = read_package_json(&path)
      .unwrap_or_else(|| panic!("Failed to read package.json from project directory: {}", path));
    let mut dependencies = Vec::new();
    for key in ["peerDependencies", "devDependencies", "dependencies"] {
      let Some(deps) = project_package_json[key].as_object() else {
        continue;
      };
      for (dep_name, _) in deps {
        if dependencies.contains(dep_name) {
          continue;
        }
        if project_json
          .iter()
          .any(|p| p["name"].as_str().unwrap_or("") == dep_name)
        {
          dependencies.push(dep_name.clone());
        }
      }
    }

    projects.push(ConfigProject {
      name,
      path: normalized_path.to_str().unwrap_or("").to_string(),
      depends_on: if dependencies.is_empty() {
        OptionConfig::None(false)
      } else {
        OptionConfig::List(dependencies)
      },
      watches: Some(vec![ConfigWatch {
        path: None,
        include: None,
        exclude: None,
        disable_default_exclude: None,
        default_exclude: vec![
          "node_modules/**".to_string(),
          "dist/**".to_string(),
          "build/**".to_string(),
          "out/**".to_string(),
          "target/**".to_string(),
          "**/*.log".to_string(),
        ],
      }]),
      commands: project_package_json["scripts"]
        .as_object()
        .unwrap_or(&serde_json::Map::new())
        .iter()
        .map(|(cmd_name, cmd_value)| {
          let cmd_str = cmd_value.as_str().unwrap_or("").to_string();
          if cmd_str.starts_with("/plexus") {
            let parts = cmd_str.split_whitespace().skip(1).map(|s| s.to_string()).collect();
            ConfigCommand::Plexus(PlexusCommand {
              name: cmd_name.clone(),
              actual_commands: parts,
            })
          } else {
            ConfigCommand::Simple(cmd_name.clone())
          }
        })
        .collect(),
      envs: None,
    });
  }

  Config {
    schema: "node_modules/plexus.schema.json".to_string(),
    version: "1.0".to_string(),
    projects,
    envs: None,
  }
}
