#!/usr/bin/env node
// Verify that user-facing documentation stays 100% synchronized with built-in
// skills (`crates/odg-core/src/skills.rs`), slash commands, tools, and privacy
// disclosures.
//
// Usage:
//   node scripts/check-docs.mjs [--root <dir>]

import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const DEFAULT_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const REQUIRED_SLASH_COMMANDS = ["/usage", "/files", "/git", "/terminal"];
const STALE_SKILL_COUNT_PHRASES = ["十四部", "十余部"];

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Convert an integer 1..=99 into `<中文数字>部` (e.g. 15 -> "十五部"). */
function toChineseSkillCountPhrase(count) {
  const digits = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
  if (count <= 0 || count >= 100) {
    return `${count}部`;
  }
  if (count < 10) {
    return `${digits[count]}部`;
  }
  const tens = Math.floor(count / 10);
  const ones = count % 10;
  const tensPrefix = tens === 1 ? "十" : `${digits[tens]}十`;
  return `${tensPrefix}${ones === 0 ? "" : digits[ones]}部`;
}

/** Return the 1-based line number of the first occurrence of `needle`, or 1 if not found. */
function findLineNumber(content, needle) {
  const idx = content.indexOf(needle);
  if (idx === -1) return 1;
  let line = 1;
  for (let i = 0; i < idx; i += 1) {
    if (content[i] === "\n") line += 1;
  }
  return line;
}

/** Parse built-in skill IDs embedded in `crates/odg-core/src/skills.rs` (`pub const BUILTINS`). */
export function parseBuiltinSkills(skillsRsContent) {
  const blockMatch = skillsRsContent.match(/pub\s+const\s+BUILTINS\s*:\s*&\[\(&str,\s*&str\)\]\s*=\s*&\[([\s\S]*?)\];/);
  if (!blockMatch) {
    return null;
  }

  const body = blockMatch[1]
    .split(/\r?\n/)
    .map((line) => line.replace(/\/\/.*$/, ""))
    .join("\n");

  const skills = [];
  const tupleRegex = /\(\s*"([a-z0-9-]+)"\s*,\s*include_str!\s*\(/g;
  for (const match of body.matchAll(tupleRegex)) {
    skills.push(match[1]);
  }
  return skills;
}

/**
 * Inspect documentation files under `rootDir` against source-of-truth definitions.
 * @param {{ rootDir?: string }} [options]
 */
export function checkDocs(options = {}) {
  const rootDir = resolve(options.rootDir ?? DEFAULT_ROOT);
  const errors = [];

  // 1. Extract built-in skill IDs from crates/odg-core/src/skills.rs
  const skillsRel = "crates/odg-core/src/skills.rs";
  const skillsAbs = join(rootDir, skillsRel);
  let builtinSkills = [];

  if (!existsSync(skillsAbs)) {
    errors.push(`${skillsRel}:1: file is missing`);
  } else {
    const parsed = parseBuiltinSkills(readFileSync(skillsAbs, "utf8"));
    if (!parsed || parsed.length === 0) {
      errors.push(`${skillsRel}:1: failed to parse non-empty pub const BUILTINS skill list`);
    } else {
      builtinSkills = parsed;
      const seen = new Set();
      for (const id of builtinSkills) {
        if (seen.has(id)) {
          errors.push(`${skillsRel}:1: duplicate built-in skill id "${id}"`);
        }
        seen.add(id);
      }
    }
  }

  const expectedCountPhrase = builtinSkills.length > 0 ? toChineseSkillCountPhrase(builtinSkills.length) : null;

  // 2. Check README.md and doc/user-guide.md for built-in skills, count phrases, and slash commands
  for (const docRel of ["README.md", "doc/user-guide.md"]) {
    const docAbs = join(rootDir, docRel);
    if (!existsSync(docAbs)) {
      errors.push(`${docRel}:1: file is missing`);
      continue;
    }
    const content = readFileSync(docAbs, "utf8");

    for (const stalePhrase of STALE_SKILL_COUNT_PHRASES) {
      if (stalePhrase !== expectedCountPhrase && content.includes(stalePhrase)) {
        const line = findLineNumber(content, stalePhrase);
        errors.push(
          `${docRel}:${line}: contains stale built-in skill count phrase "${stalePhrase}" (expected "${expectedCountPhrase ?? "updated count"}")`,
        );
      }
    }

    if (expectedCountPhrase && !content.includes(expectedCountPhrase)) {
      errors.push(
        `${docRel}:1: missing built-in skill count phrase "${expectedCountPhrase}" for ${builtinSkills.length} skills in ${skillsRel}`,
      );
    }

    for (const skillId of builtinSkills) {
      const codeTokenPattern = new RegExp(`\`/?${escapeRegExp(skillId)}\``);
      if (!codeTokenPattern.test(content)) {
        errors.push(`${docRel}:1: missing built-in skill id \`${skillId}\` (from ${skillsRel})`);
      }
    }

    for (const cmd of REQUIRED_SLASH_COMMANDS) {
      const cmdPattern = new RegExp(`\`${escapeRegExp(cmd)}(?:\\s+[^\`]+)?\``);
      if (!cmdPattern.test(content)) {
        errors.push(`${docRel}:1: missing required slash command \`${cmd}\``);
      }
    }

    for (const initAlias of ["/立规矩", "/规矩"]) {
      if (!content.includes(`\`${initAlias}\``)) {
        errors.push(`${docRel}:1: missing /init Chinese alias \`${initAlias}\``);
      }
    }

    if (!content.includes("30 秒")) {
      errors.push(`${docRel}:1: missing 30-second inline command timeout description ("30 秒")`);
    }
  }

  // 3. Check doc/TOOLS.md for batch_inspect, read_only subagents, and v1.0.0 baseline
  const toolsRel = "doc/TOOLS.md";
  const toolsAbs = join(rootDir, toolsRel);
  if (!existsSync(toolsAbs)) {
    errors.push(`${toolsRel}:1: file is missing`);
  } else {
    const toolsContent = readFileSync(toolsAbs, "utf8");
    if (!/`batch_inspect`/.test(toolsContent)) {
      errors.push(`${toolsRel}:1: missing \`batch_inspect\` tool documentation`);
    }
    if (!/`?read_only`?/.test(toolsContent)) {
      errors.push(`${toolsRel}:1: missing read-only subagent (\`read_only: true\`) documentation`);
    }
    if (!toolsContent.includes("v1.0.0")) {
      errors.push(`${toolsRel}:1: baseline version has not been updated to v1.0.0`);
    }
  }

  // 4. Check doc/privacy.md for runtime update opt-in & model/quota probing disclosures
  const privacyRel = "doc/privacy.md";
  const privacyAbs = join(rootDir, privacyRel);
  if (!existsSync(privacyAbs)) {
    errors.push(`${privacyRel}:1: file is missing`);
  } else {
    const privacyContent = readFileSync(privacyAbs, "utf8");
    for (const token of ["auto_update_runtime", "/v1/models", "agy"]) {
      if (!privacyContent.includes(token)) {
        errors.push(`${privacyRel}:1: missing required privacy disclosure token "${token}"`);
      }
    }
  }

  return {
    ok: errors.length === 0 && builtinSkills.length > 0,
    rootDir,
    builtinSkills,
    expectedCountPhrase,
    requiredSlashCommands: REQUIRED_SLASH_COMMANDS,
    errors,
  };
}

function parseCliArgs(argv) {
  let rootDir = DEFAULT_ROOT;

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") {
      return { help: true, rootDir };
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
    throw new Error(`unknown argument: ${arg}`);
  }

  return { help: false, rootDir };
}

function main() {
  let parsed;
  try {
    parsed = parseCliArgs(process.argv.slice(2));
  } catch (err) {
    console.error(`check-docs: ${err instanceof Error ? err.message : String(err)}`);
    process.exit(1);
  }

  if (parsed.help) {
    console.log("Usage: node scripts/check-docs.mjs [--root <dir>]");
    process.exit(0);
  }

  const result = checkDocs({ rootDir: parsed.rootDir });

  if (!result.ok) {
    console.error(`Docs consistency check failed (${result.errors.length} issue(s)):`);
    for (const err of result.errors) {
      console.error(`  - ${err}`);
    }
    process.exit(1);
  }

  console.log(
    `Docs consistency check passed:\n` +
      `  - Built-in skills (${result.builtinSkills.length}, "${result.expectedCountPhrase}"): ${result.builtinSkills.join(", ")}\n` +
      `  - Slash commands verified in README.md & doc/user-guide.md: ${result.requiredSlashCommands.join(", ")} (plus /init aliases /立规矩, /规矩 & 30s inline timeout)\n` +
      `  - Tool & privacy docs verified: doc/TOOLS.md (batch_inspect, read_only, v1.0.0), doc/privacy.md (auto_update_runtime, /v1/models, agy)`,
  );
}

const isDirectRun =
  typeof process.argv[1] === "string" && import.meta.url === pathToFileURL(resolve(process.argv[1])).href;

if (isDirectRun) {
  main();
}
