use std::{collections::HashMap, path::Path};

use tokio::{fs, process::Command};

#[derive(Debug, Clone)]
pub struct Project {
    _name: String,
    _path: String,
    _scripts: Vec<String>,
    _deps: Vec<String>,
}

impl Project {
    pub fn new(name: String, path: String, deps: Vec<String>, scripts: Vec<String>) -> Self {
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

impl Pnpm {
    pub async fn new(filters: &[String]) -> Result<Self, ()> {
        let mut pnpm = Self {
            _projects: HashMap::new(),
        };

        pnpm.load_projects(filters, 0).await;

        Ok(pnpm)
    }

    #[async_recursion::async_recursion]
    async fn load_projects(&mut self, projects: &[String], counter: usize) {
        if counter > 10 {
            eprintln!("Too many recursive calls to load_projects, possible circular dependency");
            return;
        }
        let output = Command::new("pnpm")
            .arg("list")
            .arg("--json")
            .arg("--depth=0")
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
            let project_package_json = fs::read_to_string(Path::new(&path).join("package.json"))
                .await
                .expect("Failed to read project package.json");
            let scripts = serde_json::from_str::<serde_json::Value>(&project_package_json)
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
    }

    fn add_deps(&mut self, json: &serde_json::Value, dep_name: &str, deps: &mut Vec<String>) {
        for dep in json[dep_name]
            .as_object()
            .unwrap_or(&serde_json::Map::new())
            .values()
        {
            let dep_version = dep["version"].as_str().unwrap_or("").to_string();
            if !dep_version.starts_with("link:") || dep_version.contains("node_modules") {
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
