use crate::cli::{SkillsAction, SkillsArgs};

pub struct Skill {
  pub slug: &'static str,
  pub description: &'static str,
  body: &'static str,
}

const GUIDE: &str = include_str!("skills/guide.md");

pub const SKILLS: &[Skill] = &[
  Skill {
    slug: "client",
    description: "Drive a running `plexus run -W` session in this pnpm monorepo: list its packages and scripts, run type checks, lints and builds through it with real exit codes, start and stop dev servers, read their output, find their ports. Use before running such scripts yourself.",
    body: include_str!("skills/client.md"),
  },
  Skill {
    slug: "client/forwarding",
    description: "Reach a dev server's port from a container or sandbox through plexus, and stop those forwards.",
    body: include_str!("skills/client/forwarding.md"),
  },
  Skill {
    slug: "client/json",
    description: "The --json output of every plexus client command.",
    body: include_str!("skills/client/json.md"),
  },
  Skill {
    slug: "client/troubleshooting",
    description: "Exit code 2, permission denied, stale socket, TCP token and other plexus client errors.",
    body: include_str!("skills/client/troubleshooting.md"),
  },
];

impl Skill {
  pub fn is_reference(&self) -> bool {
    self.slug.contains('/')
  }

  /// Top-level skills carry frontmatter so the output is a valid SKILL.md as-is.
  pub fn render(&self, cmd: &str) -> String {
    let body = self.body.replace("{{plexus}}", cmd);
    if self.is_reference() {
      return body;
    }
    format!(
      "---\nname: plexus-{}\ndescription: {}\n---\n\n{}",
      self.slug,
      serde_json::to_string(self.description).unwrap_or_default(),
      body
    )
  }
}

pub fn find(slug: &str) -> Option<&'static Skill> {
  SKILLS.iter().find(|s| s.slug == slug.trim_matches('/'))
}

pub fn guide(cmd: &str) -> String {
  GUIDE.replace("{{plexus}}", cmd)
}

pub fn run(args: SkillsArgs) -> i32 {
  let cmd = args.cmd.as_deref().unwrap_or("plexus");
  match args.action {
    SkillsAction::List { json } => {
      if json {
        let list: Vec<_> = SKILLS
          .iter()
          .map(|s| serde_json::json!({"slug": s.slug, "description": s.description, "reference": s.is_reference()}))
          .collect();
        println!("{}", serde_json::Value::Array(list));
      } else {
        let width = SKILLS.iter().map(|s| s.slug.len()).max().unwrap_or(0);
        for s in SKILLS {
          println!("{:<width$}  {}", s.slug, s.description, width = width);
        }
        println!("\nPrint one with `{} skills get <slug>`; `{} skills get` explains how to use them.", cmd, cmd);
      }
      0
    }
    SkillsAction::Get { slug: None } => {
      print!("{}", guide(cmd));
      0
    }
    SkillsAction::Get { slug: Some(slug) } => match find(&slug) {
      Some(s) => {
        print!("{}", s.render(cmd));
        0
      }
      None => {
        let slugs: Vec<&str> = SKILLS.iter().map(|s| s.slug).collect();
        eprintln!("unknown skill '{}'; available: {}", slug, slugs.join(", "));
        1
      }
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use clap::CommandFactory;

  fn all_text() -> String {
    SKILLS.iter().map(|s| s.render("plexus")).collect::<Vec<_>>().join("\n")
  }

  /// A renamed or added client subcommand or flag must show up in the docs agents read.
  #[test]
  fn every_client_subcommand_and_flag_is_documented() {
    let text = all_text();
    let cli = crate::cli::NewCli::command();
    let client = cli.find_subcommand("client").expect("client subcommand");
    let mut missing = Vec::new();
    for arg in client.get_arguments() {
      if let Some(long) = arg.get_long()
        && !text.contains(&format!("--{}", long))
      {
        missing.push(format!("client --{}", long));
      }
    }
    for sub in client.get_subcommands() {
      let name = sub.get_name();
      if !text.contains(&format!("client {}", name)) {
        missing.push(format!("client {}", name));
      }
      for arg in sub.get_arguments() {
        if let Some(long) = arg.get_long()
          && long != "help"
          && !text.contains(&format!("--{}", long))
        {
          missing.push(format!("client {} --{}", name, long));
        }
      }
    }
    assert!(missing.is_empty(), "not documented in src/skills: {:?}", missing);
  }

  #[test]
  fn every_linked_slug_exists_and_every_reference_is_linked() {
    let text = format!("{}\n{}", all_text(), guide("plexus"));
    let re = regex::Regex::new(r"skills get ([a-z/]+)").unwrap();
    for cap in re.captures_iter(&text) {
      assert!(find(&cap[1]).is_some(), "links to missing skill '{}'", &cap[1]);
    }
    for s in SKILLS.iter().filter(|s| s.is_reference()) {
      assert!(
        text.contains(&format!("skills get {}", s.slug)),
        "reference '{}' is never linked",
        s.slug
      );
    }
  }

  #[test]
  fn top_level_skills_render_as_skill_md() {
    let s = find("client").unwrap().render("pnpm exec plexus");
    let mut parts = s.splitn(3, "---\n");
    assert_eq!(parts.next(), Some(""));
    let front = parts.next().unwrap();
    assert!(front.contains("name: plexus-client\n"));
    assert!(front.contains("description: \""));
    assert!(s.contains("pnpm exec plexus client list"));
    assert!(!s.contains("{{plexus}}"));
    assert!(!find("client/json").unwrap().render("plexus").starts_with("---"));
  }

  #[test]
  fn guide_carries_the_agents_md_line() {
    let g = guide("plexus");
    assert!(g.contains("AGENTS.md"));
    assert!(g.contains("run `plexus skills get client` and follow it"));
  }
}
