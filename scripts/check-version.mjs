#!/usr/bin/env node
// Verify that every release-bearing manifest in the repository declares the
// exact same version string (and optionally matches a git tag version).
//
// Usage:
//   node scripts/check-version.mjs [--root <dir>] [<expectedTagVersion>]

import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const DEFAULT_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DEFAULT_WORKSPACE_MEMBERS = ["crates/odg-core", "crates/odg-cli", "src-tauri"];

/** Strip trailing `# ...` TOML comments that lie outside double/single quotes. */
function stripTomlComment(line) {
  let inSingle = false;
  let inDouble = false;
  let escaped = false;
  for (let i = 0; i < line.length; i += 1) {
    const ch = line[i];
    if (escaped) {
      escaped = false;
      continue;
    }
    if (inDouble && ch === "\\") {
      escaped = true;
      continue;
    }
    if (!inSingle && ch === '"') {
      inDouble = !inDouble;
      continue;
    }
    if (!inDouble && ch === "'") {
      inSingle = !inSingle;
      continue;
    }
    if (!inSingle && !inDouble && ch === "#") {
      return line.slice(0, i);
    }
  }
  return line;
}

/** Parse root `Cargo.toml` for `[workspace].members` and optional `[workspace.package].version`. */
function parseRootCargoToml(content) {
  const lines = content.split(/\r?\n/);
  let section = "";
  let workspaceVersion = null;
  let collectingMembers = false;
  let membersBuffer = "";

  for (let i = 0; i < lines.length; i += 1) {
    const cleaned = stripTomlComment(lines[i]).trim();
    if (!cleaned) continue;

    if (collectingMembers) {
      membersBuffer += ` ${cleaned}`;
      if (cleaned.includes("]")) {
        collectingMembers = false;
      }
      continue;
    }

    const headerMatch = cleaned.match(/^\[([^\]]+)\]$/);
    if (headerMatch) {
      section = headerMatch[1].trim();
      continue;
    }

    if (section === "workspace") {
      const membersStart = cleaned.match(/^members\s*=\s*(\[.*)$/);
      if (membersStart) {
        membersBuffer = membersStart[1];
        if (!membersBuffer.includes("]")) {
          collectingMembers = true;
        }
      }
    } else if (section === "workspace.package") {
      const verMatch = cleaned.match(/^version\s*=\s*"([^"]+)"\s*$/);
      if (verMatch) {
        workspaceVersion = { version: verMatch[1], line: i + 1 };
      }
    }
  }

  const members = [];
  if (membersBuffer) {
    const itemRegex = /"([^"]+)"|'([^']+)'/g;
    for (const match of membersBuffer.matchAll(itemRegex)) {
      members.push(match[1] ?? match[2]);
    }
  }

  return {
    members: members.length > 0 ? members : DEFAULT_WORKSPACE_MEMBERS,
    workspaceVersion,
  };
}

/** Parse a crate's `Cargo.toml` `[package]` version (direct string or `version.workspace = true`). */
function parseCrateCargoToml(content) {
  const lines = content.split(/\r?\n/);
  let section = "";

  for (let i = 0; i < lines.length; i += 1) {
    const cleaned = stripTomlComment(lines[i]).trim();
    if (!cleaned) continue;

    const headerMatch = cleaned.match(/^\[([^\]]+)\]$/);
    if (headerMatch) {
      section = headerMatch[1].trim();
      continue;
    }

    if (section !== "package") continue;

    const directMatch = cleaned.match(/^version\s*=\s*"([^"]+)"\s*$/);
    if (directMatch) {
      return { kind: "direct", version: directMatch[1], line: i + 1 };
    }

    if (
      /^version\.workspace\s*=\s*true\s*$/.test(cleaned) ||
      /^version\s*=\s*\{\s*workspace\s*=\s*true\s*\}\s*$/.test(cleaned)
    ) {
      return { kind: "workspace", line: i + 1 };
    }
  }

  return null;
}

/** Locate the 1-based line number of the top-level `"version"` property in a JSON document. */
function findJsonTopLevelVersionLine(content) {
  let depth = 0;
  let line = 1;
  let inString = false;
  let escaped = false;

  for (let i = 0; i < content.length; i += 1) {
    const ch = content[i];
    if (ch === "\n") {
      line += 1;
      continue;
    }
    if (inString) {
      if (escaped) {
        escaped = false;
      } else if (ch === "\\") {
        escaped = true;
      } else if (ch === '"') {
        inString = false;
      }
      continue;
    }
    if (ch === "{") {
      depth += 1;
      continue;
    }
    if (ch === "}") {
      depth -= 1;
      continue;
    }
    if (ch === '"') {
      if (depth === 1 && content.slice(i, i + 9) === '"version"') {
        const afterKey = content.slice(i + 9);
        if (/^\s*:/.test(afterKey)) {
          return line;
        }
      }
      inString = true;
    }
  }
  return 1;
}

/** Parse `pkgver=...` from an Arch Linux `PKGBUILD`. */
function parsePkgbuildVersion(content) {
  const lines = content.split(/\r?\n/);
  for (let i = 0; i < lines.length; i += 1) {
    const raw = lines[i];
    if (/^\s*#/.test(raw)) continue;
    const match = raw.match(/^\s*pkgver\s*=\s*(?:"([^"]+)"|'([^']+)'|([^\s#]+))/);
    if (match) {
      return {
        version: match[1] ?? match[2] ?? match[3],
        line: i + 1,
      };
    }
  }
  return null;
}

function normalizeTagVersion(raw) {
  const trimmed = raw.trim();
  if (/^v\d+\./.test(trimmed)) {
    return trimmed.slice(1);
  }
  return trimmed;
}

function pickMajorityVersion(entries) {
  const counts = new Map();
  for (const entry of entries) {
    counts.set(entry.version, (counts.get(entry.version) ?? 0) + 1);
  }
  const pkgEntry = entries.find((entry) => entry.file === "package.json");
  let bestVersion = pkgEntry?.version ?? entries[0]?.version ?? "";
  let bestCount = counts.get(bestVersion) ?? 0;
  for (const [version, count] of counts.entries()) {
    if (count > bestCount) {
      bestVersion = version;
      bestCount = count;
    }
  }
  return bestVersion;
}

/**
 * Inspect all version-bearing manifests under `rootDir`.
 * @param {{ rootDir?: string, expectedTagVersion?: string | null }} [options]
 */
export function checkVersions(options = {}) {
  const rootDir = resolve(options.rootDir ?? DEFAULT_ROOT);
  const expectedFromTag = options.expectedTagVersion ? normalizeTagVersion(options.expectedTagVersion) : null;

  const entries = [];
  const errors = [];

  // 1. Root Cargo.toml + workspace member Cargo.toml manifests
  const rootCargoRel = "Cargo.toml";
  const rootCargoAbs = join(rootDir, rootCargoRel);
  let members = DEFAULT_WORKSPACE_MEMBERS;
  let workspaceVersion = null;

  if (!existsSync(rootCargoAbs)) {
    errors.push(`${rootCargoRel}:1: file is missing`);
  } else {
    const parsedRoot = parseRootCargoToml(readFileSync(rootCargoAbs, "utf8"));
    members = parsedRoot.members;
    workspaceVersion = parsedRoot.workspaceVersion;
    if (workspaceVersion) {
      entries.push({
        file: rootCargoRel,
        line: workspaceVersion.line,
        version: workspaceVersion.version,
      });
    }
  }

  for (const member of members) {
    const rel = `${member.replace(/\\/g, "/")}/Cargo.toml`;
    const abs = join(rootDir, rel);
    if (!existsSync(abs)) {
      errors.push(`${rel}:1: file is missing`);
      continue;
    }
    const parsed = parseCrateCargoToml(readFileSync(abs, "utf8"));
    if (!parsed) {
      errors.push(`${rel}:1: missing [package].version`);
      continue;
    }
    if (parsed.kind === "direct") {
      entries.push({ file: rel, line: parsed.line, version: parsed.version });
    } else if (workspaceVersion) {
      entries.push({ file: rel, line: parsed.line, version: workspaceVersion.version });
    } else {
      errors.push(`${rel}:${parsed.line}: version.workspace = true requires [workspace.package].version in Cargo.toml`);
    }
  }

  // 2. package.json & src-tauri/tauri.conf.json
  for (const jsonRel of ["package.json", "src-tauri/tauri.conf.json"]) {
    const jsonAbs = join(rootDir, jsonRel);
    if (!existsSync(jsonAbs)) {
      errors.push(`${jsonRel}:1: file is missing`);
      continue;
    }
    const raw = readFileSync(jsonAbs, "utf8");
    let data;
    try {
      data = JSON.parse(raw);
    } catch (err) {
      errors.push(`${jsonRel}:1: invalid JSON (${err instanceof Error ? err.message : String(err)})`);
      continue;
    }
    if (typeof data?.version !== "string" || data.version.trim() === "") {
      errors.push(`${jsonRel}:1: missing top-level string "version"`);
      continue;
    }
    entries.push({
      file: jsonRel,
      line: findJsonTopLevelVersionLine(raw),
      version: data.version.trim(),
    });
  }

  // 3. packaging/arch/PKGBUILD
  const pkgbuildRel = "packaging/arch/PKGBUILD";
  const pkgbuildAbs = join(rootDir, pkgbuildRel);
  if (!existsSync(pkgbuildAbs)) {
    errors.push(`${pkgbuildRel}:1: file is missing`);
  } else {
    const parsedPkgbuild = parsePkgbuildVersion(readFileSync(pkgbuildAbs, "utf8"));
    if (!parsedPkgbuild) {
      errors.push(`${pkgbuildRel}:1: missing pkgver=...`);
    } else {
      entries.push({
        file: pkgbuildRel,
        line: parsedPkgbuild.line,
        version: parsedPkgbuild.version,
      });
    }
  }

  const expected = expectedFromTag ?? pickMajorityVersion(entries);
  const mismatches = entries.filter((entry) => entry.version !== expected);

  return {
    ok: errors.length === 0 && mismatches.length === 0 && entries.length > 0,
    expected,
    expectedFromTag: Boolean(expectedFromTag),
    entries,
    mismatches,
    errors,
  };
}

function parseCliArgs(argv) {
  let rootDir = DEFAULT_ROOT;
  let expectedTagVersion = null;

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") {
      return { help: true, rootDir, expectedTagVersion };
    }
    if (arg === "--root") {
      const next = argv[i + 1];
      if (!next) {
        throw new Error("--root requires a directory argument");
      }
      rootDir = resolve(next);
      i += 1;
      continue;
    }
    if (arg.startsWith("--root=")) {
      const val = arg.slice("--root=".length);
      if (!val) {
        throw new Error("--root= requires a non-empty directory argument");
      }
      rootDir = resolve(val);
      continue;
    }
    if (arg.startsWith("-")) {
      throw new Error(`unknown option: ${arg}`);
    }
    if (expectedTagVersion !== null) {
      throw new Error(`unexpected extra positional argument: ${arg}`);
    }
    expectedTagVersion = arg;
  }

  return { help: false, rootDir, expectedTagVersion };
}

function main() {
  let parsed;
  try {
    parsed = parseCliArgs(process.argv.slice(2));
  } catch (err) {
    console.error(`check-version: ${err instanceof Error ? err.message : String(err)}`);
    process.exit(1);
  }

  if (parsed.help) {
    console.log("Usage: node scripts/check-version.mjs [--root <dir>] [<expectedTagVersion>]");
    process.exit(0);
  }

  const result = checkVersions({
    rootDir: parsed.rootDir,
    expectedTagVersion: parsed.expectedTagVersion,
  });

  if (!result.ok) {
    const targetDesc = result.expectedFromTag
      ? `expected tag version "${result.expected}"`
      : `baseline version "${result.expected}"`;
    console.error(`Version check failed against ${targetDesc}:`);
    for (const err of result.errors) {
      console.error(`  - ${err}`);
    }
    for (const mismatch of result.mismatches) {
      console.error(
        `  - ${mismatch.file}:${mismatch.line}: actual "${mismatch.version}" (expected "${result.expected}")`,
      );
    }
    process.exit(1);
  }

  const summaryFiles = result.entries.map((e) => `${e.file}:${e.line}`).join(", ");
  console.log(
    `Version check passed: ${result.entries.length} manifests aligned at ${result.expected} (${summaryFiles})`,
  );
}

const isDirectRun =
  typeof process.argv[1] === "string" && import.meta.url === pathToFileURL(resolve(process.argv[1])).href;

if (isDirectRun) {
  main();
}
