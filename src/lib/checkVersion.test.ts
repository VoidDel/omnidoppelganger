import { describe, expect, it } from "vitest";

interface SpawnResult {
  status: number | null;
  stdout: string;
  stderr: string;
}

interface ChildProcessModule {
  spawnSync(command: string, args: string[], options: { encoding: "utf8" }): SpawnResult;
}

interface FsModule {
  mkdirSync(path: string, options?: { recursive?: boolean }): void;
  mkdtempSync(prefix: string): string;
  readFileSync(path: string, encoding: "utf8"): string;
  rmSync(path: string, options?: { recursive?: boolean; force?: boolean }): void;
  writeFileSync(path: string, data: string): void;
}

interface OsModule {
  tmpdir(): string;
}

interface PathModule {
  dirname(path: string): string;
  join(...paths: string[]): string;
  resolve(...paths: string[]): string;
}

interface ProcessModule {
  execPath: string;
}

interface UrlModule {
  fileURLToPath(url: string): string;
}

const importNode = <T>(specifier: string): Promise<T> => import(/* @vite-ignore */ specifier) as Promise<T>;

async function loadHarness() {
  const [cp, fs, os, path, proc, url] = await Promise.all([
    importNode<ChildProcessModule>("node:child_process"),
    importNode<FsModule>("node:fs"),
    importNode<OsModule>("node:os"),
    importNode<PathModule>("node:path"),
    importNode<ProcessModule>("node:process"),
    importNode<UrlModule>("node:url"),
  ]);

  const here = path.dirname(url.fileURLToPath(import.meta.url));
  const repoRoot = path.resolve(here, "..", "..");
  const scriptPath = path.join(repoRoot, "scripts", "check-version.mjs");

  const runCheckVersion = (args: string[] = []): SpawnResult =>
    cp.spawnSync(proc.execPath, [scriptPath, ...args], { encoding: "utf8" });

  const withTempRepo = (files: Record<string, string>, fn: (dir: string) => void) => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "odg-check-version-"));
    try {
      for (const [rel, content] of Object.entries(files)) {
        const full = path.join(dir, rel);
        fs.mkdirSync(path.dirname(full), { recursive: true });
        fs.writeFileSync(full, content);
      }
      fn(dir);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  };

  return { fs, path, repoRoot, runCheckVersion, withTempRepo };
}

function buildFixtureFiles(overrides: Partial<Record<string, string>> = {}): Record<string, string> {
  const base: Record<string, string> = {
    "Cargo.toml": ['[workspace]', 'resolver = "3"', 'members = ["crates/odg-core", "crates/odg-cli", "src-tauri"]', ""].join(
      "\n",
    ),
    "crates/odg-core/Cargo.toml": ['[package]', 'name = "odg-core"', 'version = "0.8.0"', 'edition = "2024"', ""].join(
      "\n",
    ),
    "crates/odg-cli/Cargo.toml": ['[package]', 'name = "odg-cli"', 'version = "0.8.0"', 'edition = "2024"', ""].join(
      "\n",
    ),
    "src-tauri/Cargo.toml": [
      "[package]",
      'name = "omnidoppelganger"',
      'version = "0.8.0"',
      'edition = "2024"',
      "",
    ].join("\n"),
    "package.json": ['{', '  "name": "omnidoppelganger",', '  "private": true,', '  "version": "0.8.0"', "}", ""].join(
      "\n",
    ),
    "src-tauri/tauri.conf.json": [
      "{",
      '  "$schema": "https://schema.tauri.app/config/2",',
      '  "productName": "OmniDoppelganger",',
      '  "version": "0.8.0"',
      "}",
      "",
    ].join("\n"),
    "packaging/arch/PKGBUILD": [
      "# Maintainer: OmniDoppelganger Contributors",
      "pkgname=omnidoppelganger",
      "pkgver=0.8.0",
      "pkgrel=1",
      "",
    ].join("\n"),
  };

  for (const [key, value] of Object.entries(overrides)) {
    if (typeof value === "string") {
      base[key] = value;
    }
  }
  return base;
}

describe("scripts/check-version.mjs", () => {
  it("exits 0 on the real repository root when all manifests align", async () => {
    const { fs, path, repoRoot, runCheckVersion } = await loadHarness();
    const pkg = JSON.parse(fs.readFileSync(path.join(repoRoot, "package.json"), "utf8")) as { version: string };

    const res = runCheckVersion(["--root", repoRoot]);
    expect(res.status).toBe(0);
    expect(res.stderr).toBe("");
    expect(res.stdout).toContain(`aligned at ${pkg.version}`);

    const withMatchingTag = runCheckVersion(["--root", repoRoot, `v${pkg.version}`]);
    expect(withMatchingTag.status).toBe(0);
  });

  it("exits non-zero and prints exact file path, line number, and value when tauri.conf.json diverges", async () => {
    const { runCheckVersion, withTempRepo } = await loadHarness();
    const mismatchedTauriConf = [
      "{",
      '  "$schema": "https://schema.tauri.app/config/2",',
      '  "productName": "OmniDoppelganger",',
      '  "version": "0.8.1"',
      "}",
      "",
    ].join("\n");

    withTempRepo(buildFixtureFiles({ "src-tauri/tauri.conf.json": mismatchedTauriConf }), (tempRoot) => {
      const res = runCheckVersion(["--root", tempRoot]);
      expect(res.status).toBe(1);
      expect(res.stderr).toContain("src-tauri/tauri.conf.json:4");
      expect(res.stderr).toContain('"0.8.1"');
      expect(res.stderr).toContain('"0.8.0"');
    });
  });

  it("blocks release when expected git tag version does not match repository manifests", async () => {
    const { repoRoot, runCheckVersion } = await loadHarness();

    const res = runCheckVersion(["--root", repoRoot, "9.9.9"]);
    expect(res.status).toBe(1);
    expect(res.stderr).toContain('expected tag version "9.9.9"');
    expect(res.stderr).toContain("package.json:");
    expect(res.stderr).toContain("src-tauri/tauri.conf.json:");
    expect(res.stderr).toContain("packaging/arch/PKGBUILD:");
  });

  it("supports [workspace.package] version inheritance with version.workspace = true", async () => {
    const { runCheckVersion, withTempRepo } = await loadHarness();
    const inheritedFiles = buildFixtureFiles({
      "Cargo.toml": [
        "[workspace]",
        'resolver = "3"',
        'members = ["crates/odg-core", "crates/odg-cli", "src-tauri"]',
        "",
        "[workspace.package]",
        'version = "1.0.0"',
        "",
      ].join("\n"),
      "crates/odg-core/Cargo.toml": ['[package]', 'name = "odg-core"', "version.workspace = true", ""].join("\n"),
      "crates/odg-cli/Cargo.toml": ['[package]', 'name = "odg-cli"', "version = { workspace = true }", ""].join("\n"),
      "src-tauri/Cargo.toml": ['[package]', 'name = "omnidoppelganger"', "version.workspace = true", ""].join("\n"),
      "package.json": ['{', '  "name": "omnidoppelganger",', '  "version": "1.0.0"', "}", ""].join("\n"),
      "src-tauri/tauri.conf.json": ['{', '  "productName": "OmniDoppelganger",', '  "version": "1.0.0"', "}", ""].join(
        "\n",
      ),
      "packaging/arch/PKGBUILD": ["pkgname=omnidoppelganger", "pkgver=1.0.0", "pkgrel=1", ""].join("\n"),
    });

    withTempRepo(inheritedFiles, (tempRoot) => {
      const ok = runCheckVersion(["--root", tempRoot, "1.0.0"]);
      expect(ok.status).toBe(0);
      expect(ok.stdout).toContain("aligned at 1.0.0");
    });

    withTempRepo(
      {
        ...inheritedFiles,
        "packaging/arch/PKGBUILD": ["# Arch build", "pkgname=omnidoppelganger", "pkgver=0.9.0", "pkgrel=1", ""].join(
          "\n",
        ),
      },
      (tempRoot) => {
        const fail = runCheckVersion(["--root", tempRoot]);
        expect(fail.status).toBe(1);
        expect(fail.stderr).toContain("packaging/arch/PKGBUILD:3");
        expect(fail.stderr).toContain('"0.9.0"');
      },
    );
  });
});
