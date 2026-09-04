use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Environment variable that lets a user point Plexus at a specific pnpm binary.
pub const PNPM_OVERRIDE_ENV: &str = "PLEXUS_PNPM";

static PNPM: OnceLock<Result<PathBuf, String>> = OnceLock::new();

fn resolve() -> Result<PathBuf, String> {
  if let Some(custom) = std::env::var_os(PNPM_OVERRIDE_ENV) {
    let custom = PathBuf::from(custom);
    if custom.is_file() {
      return Ok(custom);
    }
    // Treat a bare name in the override as something to look up on PATH.
    return which::which(&custom).map_err(|e| {
      format!(
        "{} is set to {:?} but it could not be used: {}",
        PNPM_OVERRIDE_ENV, custom, e
      )
    });
  }

  // which respects PATHEXT on Windows, so it finds pnpm.cmd / pnpm.exe / the corepack
  // shim. std::process::Command only ever appends .exe when it searches PATH, which is
  // why spawning a bare "pnpm" fails on a normal Windows install.
  which::which("pnpm").map_err(|e| {
    format!(
      "could not find pnpm on PATH ({}). Install pnpm, or set {} to the full path of the pnpm executable.",
      e, PNPM_OVERRIDE_ENV
    )
  })
}

/// The resolved absolute path to pnpm, or a message explaining why it could not be found.
/// Resolution happens once per process.
pub fn pnpm() -> Result<&'static Path, &'static str> {
  match PNPM.get_or_init(resolve) {
    Ok(p) => Ok(p.as_path()),
    Err(e) => Err(e.as_str()),
  }
}

/// The resolved pnpm path, aborting with a readable message if it is missing. Use this at
/// startup paths where there is nothing sensible to fall back to.
pub fn pnpm_or_exit() -> &'static Path {
  match pnpm() {
    Ok(p) => p,
    Err(e) => {
      eprintln!("plexus: {}", e);
      std::process::exit(1);
    }
  }
}
