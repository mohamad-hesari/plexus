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

// Every platform/arch pair we publish a binary for. Anything outside this list is not a
// broken install, it is a target we do not build, and the message should say so.
const SUPPORTED = [
  "linux-x64",
  "linux-musl-x64",
  "darwin-arm64",
  "win32-x64",
];

function isSupported() {
  return SUPPORTED.includes(`${pkgPlatform}-${arch}`);
}

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

/**
 * Why findBinary() came back empty, written for someone who has to fix it.
 */
function explainMissingBinary() {
  if (!isSupported()) {
    return [
      `Plexus has no prebuilt binary for ${platform} ${arch}.`,
      `Published targets: ${SUPPORTED.join(", ")}.`,
      "Build from source with `cargo build --release` and point at the binary yourself.",
    ].join("\n");
  }
  return [
    `Plexus could not find its ${pkgPlatform}-${arch} binary package.`,
    `Looked for ${normalName} and ${githubName}.`,
    "",
    "This almost always means the optional dependency was skipped at install time.",
    "npm records optional dependencies per platform, so a lockfile generated on one OS",
    "can leave this package out on another. To fix it:",
    "",
    "  rm -rf node_modules package-lock.json && npm install",
    "",
    `or install the package directly:  npm install ${normalName}`,
  ].join("\n");
}

module.exports = {
  githubName,
  normalName,
  findBinary,
  isSupported,
  explainMissingBinary,
};
