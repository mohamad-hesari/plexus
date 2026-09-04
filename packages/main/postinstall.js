#!/usr/bin/env node

// Generating the config schema is a convenience, not a requirement. Nothing in here may
// fail the install: a non-zero exit from a postinstall script aborts `npm install` for the
// whole project, and this used to happen on Windows whenever the platform binary package
// was skipped.

const { spawn } = require("child_process");
const fs = require("fs");
const path = require("path");
const { findBinary, explainMissingBinary } = require("./find_binary");

function warn(message) {
  console.warn(`plexus: ${message}`);
}

try {
  const binaryPath = findBinary();

  if (!binaryPath) {
    warn(explainMissingBinary());
    warn("Skipping config schema generation.");
    process.exitCode = 0;
  } else {
    const projectRoot = process.env.INIT_CWD || process.cwd();
    const targetFolder = path.join(projectRoot, "node_modules");

    if (!fs.existsSync(targetFolder)) {
      warn(`No node_modules at ${targetFolder}, skipping config schema generation.`);
      process.exitCode = 0;
    } else {
      const outputPath = path.join(targetFolder, "plexus.schema.json");
      const child = spawn(binaryPath, ["print-schema", "--output", outputPath], {
        stdio: "inherit",
        windowsHide: true,
      });

      child.on("error", (err) => {
        warn(`Could not run ${binaryPath}: ${err.message}`);
        warn("Skipping config schema generation.");
        process.exitCode = 0;
      });

      child.on("exit", (code) => {
        if (code === 0) {
          console.log(`plexus: config schema written to ${outputPath}`);
        } else {
          warn(`Config schema generation exited with ${code}, continuing anyway.`);
        }
        process.exitCode = 0;
      });
    }
  }
} catch (error) {
  warn(`Config schema generation failed: ${error.message}`);
  process.exitCode = 0;
}
