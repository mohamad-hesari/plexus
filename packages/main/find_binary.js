const fs = require("fs");

const platform = process.platform;
const arch = process.arch;
let pkgPlatform = platform;

// Alpine/Musl detection
if (platform === "linux" && fs.existsSync("/etc/alpine-release")) {
  pkgPlatform = "linux-musl";
}

const baseName = `plexus-${pkgPlatform}-${arch}`;
const githubName = `@mohamad-hesari/${baseName}`;
const normalName = `@m.hesari/${baseName}`;

const exe = process.platform === "win32" ? "plexus.exe" : "plexus";

function findBinary() {
  try {
    return require.resolve(`${githubName}/bin/${exe}`);
  } catch (e) {
    try {
      return require.resolve(`${normalName}/bin/${exe}`);
    } catch (e2) {
      return null;
    }
  }
}

module.exports = {
  githubName,
  normalName,
  findBinary,
};
