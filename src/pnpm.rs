use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::PathBuf;
use std::{collections::HashMap, path::Path};
use tokio::{fs, process::Command};
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
  _name: String,
  _path: String,
  _scripts: Vec<String>,
  _deps: Vec<String>,
}

impl Project {
  pub fn new(
    name: String,
    path: String,
    deps: Vec<String>,
    scripts: Vec<String>,
  ) -> Self {
    Self {
      _name: name,
      _path: path,
      _deps: deps,
      _scripts: scripts,
    }
  }

  pub fn name(&self) -> &str {
    &self._name
  }

  pub fn path(&self) -> &str {
    &self._path
  }

  pub fn deps(&self) -> &[String] {
    &self._deps
  }

  pub fn scripts(&self) -> &[String] {
    &self._scripts
  }
}

#[derive(Debug, Clone)]
pub struct Pnpm {
  _projects: HashMap<String, Project>,
}

impl Default for Pnpm {
  fn default() -> Self {
    Self::new()
  }
}

impl Pnpm {
  pub fn new() -> Self {
    Self {
      _projects: HashMap::new(),
    }
  }

  #[async_recursion::async_recursion]
  pub async fn load_projects(&mut self, projects: &[String], counter: usize) {
    if counter == 0 {
      match get_file_sha256("pnpm-lock.yaml") {
        Ok(hash) => {
          debug!("pnpm-lock.yaml hash: {}", hash);
          let mut final_vec = projects.to_vec();
          final_vec.push(hash);
          let project_hash = hash_vec_strings(final_vec);
          let mut path = PathBuf::from("node_modules");
          path.push(".plexus");
          path.push(project_hash);
          if path.exists() {
            debug!(
              "Cache hit for pnpm projects, loading from {}",
              path.display()
            );
            match fs::read_to_string(&path).await {
              Ok(contents) => {
                let cached_projects: Vec<Project> =
                  serde_json::from_str(&contents)
                    .expect("Failed to parse cached projects");
                for project in cached_projects {
                  self._projects.insert(project.name().to_string(), project);
                }
                return;
              }
              Err(e) => eprintln!("Error reading cache file: {}", e),
            }
          } else {
            debug!("Cache miss for pnpm projects, will load from pnpm list");
          }
        }
        Err(e) => eprintln!("Error reading file: {}", e),
      }
    }
    if counter > 10 {
      eprintln!(
        "Too many recursive calls to load_projects, possible circular dependency"
      );
      return;
    }
    let output = Command::new("pnpm")
      .arg("list")
      .arg("--json")
      .arg("--depth=4")
      .arg("--long")
      .args(projects.iter().flat_map(|f| vec!["--filter", f]))
      .output()
      .await
      .expect("Failed to execute pnpm list");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
      serde_json::from_str(&stdout).expect("Failed to parse pnpm list output");

    for project in json.as_array().unwrap_or(&vec![]) {
      let name = project["name"].as_str().unwrap_or("").to_string();
      let path = project["path"].as_str().unwrap_or("").to_string();
      let project_package_json =
        fs::read_to_string(Path::new(&path).join("package.json"))
          .await
          .expect("Failed to read project package.json");
      let scripts =
        serde_json::from_str::<serde_json::Value>(&project_package_json)
          .expect("Failed to parse project package.json")["scripts"]
          .as_object()
          .unwrap_or(&serde_json::Map::new())
          .keys()
          .cloned()
          .collect::<Vec<String>>();
      let mut deps = vec![];
      self.add_deps(project, "dependencies", &mut deps);
      self.add_deps(project, "devDependencies", &mut deps);
      let project = Project::new(name.clone(), path, deps.clone(), scripts);
      self._projects.insert(name, project);

      let not_found_deps: Vec<String> = deps
        .clone()
        .iter()
        .filter(|dep| !self._projects.contains_key(*dep))
        .cloned()
        .collect();
      if !not_found_deps.is_empty() {
        self.load_projects(&not_found_deps, counter + 1).await;
      }
    }

    if counter == 0 {
      // Cache the loaded projects
      let projects_vec: Vec<Project> =
        self._projects.values().cloned().collect();
      let cache_path =
        PathBuf::from("node_modules")
          .join(".plexus")
          .join(hash_vec_strings(
            [
              projects.to_vec(),
              vec![get_file_sha256("pnpm-lock.yaml").unwrap_or_default()],
            ]
            .concat(),
          ));
      if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)
          .await
          .expect("Failed to create cache directory");
      }
      match fs::write(
        &cache_path,
        serde_json::to_string(&projects_vec).unwrap(),
      )
      .await
      {
        Ok(_) => debug!("Cached pnpm projects to {}", cache_path.display()),
        Err(e) => eprintln!("Error writing cache file: {}", e),
      }
    }
  }

  fn add_deps(
    &mut self,
    json: &serde_json::Value,
    dep_name: &str,
    deps: &mut Vec<String>,
  ) {
    for dep in json[dep_name]
      .as_object()
      .unwrap_or(&serde_json::Map::new())
      .values()
    {
      let dep_version = dep["version"].as_str().unwrap_or("").to_string();
      if !dep_version.starts_with("link:")
        || dep_version.contains("node_modules")
      {
        continue;
      }

      let dep_name = dep["from"].as_str().unwrap_or("").to_string();
      deps.push(dep_name);
    }
  }

  pub fn projects(&self) -> &HashMap<String, Project> {
    &self._projects
  }
}

fn get_file_sha256(path: &str) -> io::Result<String> {
  let file = File::open(path)?;
  let mut reader = BufReader::new(file);
  let mut hasher = Sha256::new();
  let mut buffer = [0; 8192]; // 8KB chunks

  loop {
    let count = reader.read(&mut buffer)?;
    if count == 0 {
      break;
    }
    hasher.update(&buffer[..count]);
  }

  let result = hasher.finalize();
  Ok(hex::encode(result))
}

fn hash_vec_strings(strings: Vec<String>) -> String {
  let mut hasher = Sha256::new();

  for s in strings {
    hasher.update(s.as_bytes());
    // Optional: hasher.update(b"\n"); // Add delimiter for safety
  }

  let result = hasher.finalize();
  hex::encode(result)
}
