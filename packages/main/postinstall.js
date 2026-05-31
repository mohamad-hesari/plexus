#!/usr/bin/env node
const { spawn } = require("child_process");
const fs = require("fs");
const path = require("path");
const { findBinary, githubName, normalName } = require("./find_binary");

const binaryPath = findBinary();

if (!binaryPath) {
  console.error(
    `Error: Plexus could not find binary package: ${normalName} or ${githubName}`,
  );
  process.exit(1);
}

try {
  const projectRoot = process.env.INIT_CWD || process.cwd();
  const targetFolder = path.join(projectRoot, "node_modules");
  if (!fs.existsSync(targetFolder)) {
    console.error(
      "⚠️ Target folder for config schema does not exist:",
      targetFolder,
    );
    process.exit(1);
  }
  const outputPath = path.join(targetFolder, "plexus.schema.json");
  const child = spawn(binaryPath, ["print-schema", "--output", outputPath], {
    stdio: "inherit",
  });
  child.on("exit", (code) => {
    if (code === 0) {
      console.log("✅ Config schema generated successfully at:", outputPath);
    }
    process.exit(code || 0);
  });
} catch (error) {
  console.error(
    "⚠️ Failed to generate config schema during postinstall:",
    error.message,
  );
}
