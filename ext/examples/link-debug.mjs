import { copyFileSync, existsSync, mkdirSync, rmSync, symlinkSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const examplesDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(examplesDir, "..", "..");
const exeSuffix = process.platform === "win32" ? ".exe" : "";

function pickTargetBinary(name, buildCmd) {
  for (const profile of ["release", "ci", "debug"]) {
    const candidate = join(repoRoot, "target", profile, `${name}${exeSuffix}`);
    if (existsSync(candidate)) return candidate;
  }
  console.error(`examples: no ${name} binary found under ${join(repoRoot, "target")}`);
  console.error(`build it with: ${buildCmd}`);
  process.exit(1);
}

function linkBinary(targetPath, dest) {
  mkdirSync(dirname(dest), { recursive: true });
  rmSync(dest, { force: true });
  symlinkSync(relative(dirname(dest), targetPath), dest);
}

const tsrsBin = pickTargetBinary("tsrs", "cargo build -p tsrs_cli --bin tsrs");
const tscExtBin = pickTargetBinary("tsc-ext", "cargo build -p ts_ext_syntax --bin tsc-ext");

linkBinary(tsrsBin, join(examplesDir, "..", "typescript", "bin", "tsc"));
linkBinary(tscExtBin, join(repoRoot, "crates", "ts_ext_syntax", "bin", "tsc-ext"));

const dylibName =
  process.platform === "darwin"
    ? "libts_ext_syntax.dylib"
    : process.platform === "win32"
      ? "ts_ext_syntax.dll"
      : "libts_ext_syntax.so";
for (const profile of ["release", "ci", "debug"]) {
  const dylib = join(repoRoot, "target", profile, dylibName);
  if (existsSync(dylib)) {
    const addon = join(
      repoRoot,
      "crates",
      "ts_ext_syntax",
      `tsc-ext.${process.platform}-${process.arch}.node`,
    );
    copyFileSync(dylib, addon);
    break;
  }
}
