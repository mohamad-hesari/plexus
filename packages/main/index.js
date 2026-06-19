#!/usr/bin/env node

const { spawn } = require("child_process");
const { findBinary, githubName, normalName } = require("./find_binary");

const binaryPath = findBinary();

if (binaryPath) {
  const child = spawn(binaryPath, process.argv.slice(2), { stdio: "inherit" });
  child.on("exit", (code) => process.exit(code || 0));
} else {
  console.error(
    `Error: Plexus could not find binary package: ${normalName} or ${githubName}`,
  );
  process.exit(1);
}
