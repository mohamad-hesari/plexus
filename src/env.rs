use std::{
  collections::HashMap,
  path::{Path, PathBuf},
};

use tokio::fs;

pub struct Env {
  _env_key_matcher: regex::Regex,
}

impl Env {
  pub fn new() -> Self {
    Self {
      _env_key_matcher: regex::Regex::new(r"^\s*([A-Z_][A-Z0-9_]*)\s*=([^\n]*)").unwrap(),
    }
  }

  async fn get_env_from_file(&self, path: &Path, envs: &mut HashMap<String, String>) {
    let local_path = PathBuf::from(format!("{}.local", path.display()));
    let paths = vec![local_path, path.to_path_buf()];
    for path in paths {
      if !path.exists() {
        continue;
      }
      let read_to_string = fs::read_to_string(path).await;
      if let Ok(contents) = read_to_string {
        for line in contents.lines() {
          let line_trimmed = line.trim();
          if line_trimmed.is_empty() || line_trimmed.starts_with("#") {
            continue;
          }
          if let Some(caps) = self._env_key_matcher.captures(line) {
            let key = &caps[1];
            let value = &caps[2].trim();
            envs.insert(key.to_string(), value.to_string());
          }
        }
      }
    }
  }

  pub async fn get_envs(&self, path: &str) -> HashMap<String, String> {
    self.get_envs_with_specific(path, None).await
  }

  pub async fn get_envs_with_specific(&self, path: &str, specific: Option<&str>) -> HashMap<String, String> {
    let mut envs = HashMap::new();
    envs.insert("PLEXUS".to_string(), "1".to_string());
    let current_path = std::env::current_dir()
      .unwrap_or_else(|_| panic!("Failed to get current directory for task"))
      .display()
      .to_string();
    if let Some(specific) = specific {
      self
        .get_env_from_file(
          Path::new(current_path.as_str())
            .join(format!(".env.{}", specific))
            .as_path(),
          &mut envs,
        )
        .await;
    }
    self
      .get_env_from_file(Path::new(current_path.as_str()).join(".env").as_path(), &mut envs)
      .await;
    if let Some(specific) = specific {
      self
        .get_env_from_file(Path::new(path).join(format!(".env.{}", specific)).as_path(), &mut envs)
        .await;
    }
    self
      .get_env_from_file(Path::new(path).join(".env").as_path(), &mut envs)
      .await;
    envs
  }
}
