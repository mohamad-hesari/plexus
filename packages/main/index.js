#!/usr/bin/env node

const { spawn } = require("child_process");
const { findBinary, explainMissingBinary } = require("./find_binary");

const binaryPath = findBinary();

if (!binaryPath) {
  console.error(explainMissingBinary());
  process.exit(1);
}

const child = spawn(binaryPath, process.argv.slice(2), {
  stdio: "inherit",
  windowsHide: true,
});

// Plexus runs a TUI and owns the terminal. Let the child see the signal and restore the
// screen itself instead of node dying first and leaving the alternate screen behind.
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(signal, () => {});
}

child.on("error", (err) => {
  console.error(`Error: Plexus could not run ${binaryPath}: ${err.message}`);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    // Report a signal death the way a shell would, rather than as success.
    process.exit(128 + (require("os").constants.signals[signal] || 0));
  }
  process.exit(code === null ? 1 : code);
});
