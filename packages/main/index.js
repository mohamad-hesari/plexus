#!/usr/bin/env node
const { spawn } = require("child_process");
const fs = require("fs");

const platform = process.platform;
const arch = process.arch;
let pkgPlatform = platform;

// Check for Musl (Alpine)
if (platform === "linux") {
  try {
    // Alpine usually has this file, or you can check ldd version
    if (
      fs.existsSync("/etc/alpine-release") ||
      (process.report &&
        process.report.getReport().header.glibcVersionRuntime === undefined)
    ) {
      pkgPlatform = "linux-musl";
    }
  } catch (e) {
    // Fallback to standard linux if check fails
    pkgPlatform = "linux";
  }
}

const packageName = `plexus-${pkgPlatform}-${arch}`;

try {
  const binaryPath = require.resolve(`${packageName}/bin/plexus`);
  const child = spawn(binaryPath, process.argv.slice(2), { stdio: "inherit" });
  child.on("exit", (code) => process.exit(code || 0));
} catch (err) {
  console.error(
    `Error: Plexus could not find a compatible binary for ${platform}-${arch}.`,
  );
  console.error(`Attempted to load: ${packageName}`);
  process.exit(1);
}
