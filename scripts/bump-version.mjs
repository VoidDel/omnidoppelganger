#!/usr/bin/env node
// Synchronize the release version across all repository manifests:
//   1. Root `Cargo.toml` (`[workspace.package].version`)
//   2. `package.json` (`"version"`)
//   3. `src-tauri/tauri.conf.json` (`"version"`)
//   4. `packaging/arch/PKGBUILD` (`pkgver=...`, plus `packaging/arch/.SRCINFO` if present)
//
// Usage:
//   node scripts/bump-version.mjs [--root <dir>] <version>

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { checkVersions } from "./check-version.mjs";

const DEFAULT_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SEMVER_REGEX = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

function normalizeVersion(raw) {
  const cleaned = raw.trim().replace(/^[vV]/, "");
  if (!SEMVER_REGEX.test(cleaned)) {
    throw new Error(`invalid semantic version "${raw}" (expected e.g. 1.0.0 or 1.0.0-rc.1)`);
  }
  return cleaned;
}

function updateRootCargoToml(content, version) {
  const eol = content.includes("\r\n") ? "\r\n" : "\n";
  const lines = content.split(/\r?\n/);
  let section = "";
  let updated = false;

  for (let i = 0; i < lines.length; i += 1) {
    const trimmed = lines[i].replace(/#.*$/, "").trim();
    const headerMatch = trimmed.match(/^\[([^\]]+)\]$/);
    if (headerMatch) {
      section = headerMatch[1].trim();
      continue;
    }
    if (section === "workspace.package" && /^version\s*=\s*"[^"]*"/.test(trimmed)) {
      lines[i] = lines[i].replace(/^(\s*version\s*=\s*)"[^"]*"/, `$1"${version}"`);
      updated = true;
      break;
    }
  }

  if (!updated) {
    throw new Error("Cargo.toml is missing [workspace.package] version = \"...\"");
  }
  return lines.join(eol);
}

function updateJsonTopLevelVersion(content, version, relPath) {
  const parsed = JSON.parse(content);
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error(`${relPath} root must be a JSON object`);
  }
  const eol = content.includes("\r\n") ? "\r\n" : "\n";
  const replaced = content.replace(
    /^(\s*"version"\s*:\s*)"[^"]*"/m,
    `$1"${version}"`,
  );
  const check = JSON.parse(replaced);
  if (check.version !== version) {
    parsed.version = version;
    return `${JSON.stringify(parsed, null, 2)}${eol}`;
  }
  return replaced;
}

function updatePkgbuild(content, version) {
  const eol = content.includes("\r\n") ? "\r\n" : "\n";
  const pkgverSafe = version.replace(/-/g, "_");
  const lines = content.split(/\r?\n/);
  let updated = false;

  for (let i = 0; i < lines.length; i += 1) {
    if (/^\s*#/.test(lines[i])) continue;
    if (/^\s*pkgver\s*=/.test(lines[i])) {
      lines[i] = `pkgver=${pkgverSafe}`;
      updated = true;
      break;
    }
  }

  if (!updated) {
    throw new Error("packaging/arch/PKGBUILD is missing pkgver=...");
  }
  return lines.join(eol);
}

function updateSrcinfo(content, oldPkgver, nextVersion) {
  const eol = content.includes("\r\n") ? "\r\n" : "\n";
  const pkgverSafe = nextVersion.replace(/-/g, "_");
  let out = content.replace(/^(\s*pkgver\s*=\s*)\S+/gm, `$1${pkgverSafe}`);
  if (oldPkgver && oldPkgver !== pkgverSafe) {
    out = out.replaceAll(`omnidoppelganger-${oldPkgver}.tar.gz`, `omnidoppelganger-${pkgverSafe}.tar.gz`);
    out = out.replaceAll(`/refs/tags/v${oldPkgver}.tar.gz`, `/refs/tags/v${pkgverSafe}.tar.gz`);
  }
  return out.replace(/\r?\n/g, eol);
}

/**
 * Synchronize version across `Cargo.toml`, `package.json`, `src-tauri/tauri.conf.json`, and `packaging/arch/PKGBUILD`.
 * @param {{ rootDir?: string, version: string }} options
 */
export function bumpVersion(options) {
  const rootDir = resolve(options.rootDir ?? DEFAULT_ROOT);
  const version = normalizeVersion(options.version);
  const touched = [];

  // 1. Root Cargo.toml ([workspace.package])
  const cargoRel = "Cargo.toml";
  const cargoAbs = join(rootDir, cargoRel);
  writeFileSync(cargoAbs, updateRootCargoToml(readFileSync(cargoAbs, "utf8"), version), "utf8");
  touched.push(cargoRel);

  // 2. package.json & src-tauri/tauri.conf.json
  for (const jsonRel of ["package.json", "src-tauri/tauri.conf.json"]) {
    const jsonAbs = join(rootDir, jsonRel);
    writeFileSync(jsonAbs, updateJsonTopLevelVersion(readFileSync(jsonAbs, "utf8"), version, jsonRel), "utf8");
    touched.push(jsonRel);
  }

  // 3. packaging/arch/PKGBUILD (+ optional .SRCINFO)
  const pkgbuildRel = "packaging/arch/PKGBUILD";
  const pkgbuildAbs = join(rootDir, pkgbuildRel);
  const prevPkgbuild = readFileSync(pkgbuildAbs, "utf8");
  const prevVerMatch = prevPkgbuild.match(/^\s*pkgver\s*=\s*([^\s#]+)/m);
  const prevPkgver = prevVerMatch ? prevVerMatch[1].replace(/^['"]|['"]$/g, "") : null;
  writeFileSync(pkgbuildAbs, updatePkgbuild(prevPkgbuild, version), "utf8");
  touched.push(pkgbuildRel);

  const srcinfoRel = "packaging/arch/.SRCINFO";
  const srcinfoAbs = join(rootDir, srcinfoRel);
  if (existsSync(srcinfoAbs)) {
    writeFileSync(srcinfoAbs, updateSrcinfo(readFileSync(srcinfoAbs, "utf8"), prevPkgver, version), "utf8");
    touched.push(srcinfoRel);
  }

  const check = checkVersions({ rootDir, expectedTagVersion: version.replace(/-/g, "_") === version ? version : null });
  if (!check.ok && version.indexOf("-") === -1) {
    throw new Error(`post-bump verification failed:\n${[...check.errors, ...check.mismatches.map((m) => `${m.file}:${m.line}: ${m.version}`)].join("\n")}`);
  }

  return { version, touched };
}

function parseCliArgs(argv) {
  let rootDir = DEFAULT_ROOT;
  let version = null;

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") {
      return { help: true, rootDir, version };
    }
    if (arg === "--root") {
      const next = argv[i + 1];
      if (!next) throw new Error("--root requires a directory argument");
      rootDir = resolve(next);
      i += 1;
      continue;
    }
    if (arg.startsWith("--root=")) {
      const val = arg.slice("--root=".length);
      if (!val) throw new Error("--root= requires a non-empty directory argument");
      rootDir = resolve(val);
      continue;
    }
    if (arg.startsWith("-")) {
      throw new Error(`unknown option: ${arg}`);
    }
    if (version !== null) {
      throw new Error(`unexpected extra argument: ${arg}`);
    }
    version = arg;
  }

  if (!version) {
    throw new Error("missing <version> argument (usage: node scripts/bump-version.mjs <version>)");
  }
  return { help: false, rootDir, version };
}

function main() {
  let parsed;
  try {
    parsed = parseCliArgs(process.argv.slice(2));
  } catch (err) {
    console.error(`bump-version: ${err instanceof Error ? err.message : String(err)}`);
    process.exit(1);
  }

  if (parsed.help) {
    console.log("Usage: node scripts/bump-version.mjs [--root <dir>] <version>");
    process.exit(0);
  }

  try {
    const result = bumpVersion({ rootDir: parsed.rootDir, version: parsed.version });
    console.log(`Bumped version to ${result.version} in: ${result.touched.join(", ")}`);
  } catch (err) {
    console.error(`bump-version: ${err instanceof Error ? err.message : String(err)}`);
    process.exit(1);
  }
}

const isDirectRun =
  typeof process.argv[1] === "string" && import.meta.url === pathToFileURL(resolve(process.argv[1])).href;

if (isDirectRun) {
  main();
}
