#!/usr/bin/env node
const { spawn } = require("child_process");
const { findBinary, githubName, normalName } = require("./find_binary");
// const fs = require("fs");
//
// const platform = process.platform;
// const arch = process.arch;
// let pkgPlatform = platform;
//
// // Alpine/Musl detection
// if (platform === "linux" && fs.existsSync("/etc/alpine-release")) {
//   pkgPlatform = "linux-musl";
// }
//
// const baseName = `plexus-${pkgPlatform}-${arch}`;
// const githubName = `@mohamad-hesari/${baseName}`;
// const normalName = `@m.hesari/${baseName}`;
//
// function findBinary() {
//   try {
//     return require.resolve(`${githubName}/bin/plexus`);
//   } catch (e) {
//     try {
//       return require.resolve(`${normalName}/bin/plexus`);
//     } catch (e2) {
//       return null;
//     }
//   }
// }

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
