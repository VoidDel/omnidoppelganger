import { describe, expect, it } from "vitest";

interface DirentLike {
  name: string;
  isDirectory(): boolean;
}

interface FsModule {
  readdirSync(path: string, options: { withFileTypes: true }): DirentLike[];
  readFileSync(path: string, encoding: "utf8"): string;
}

interface PathModule {
  dirname(path: string): string;
  extname(path: string): string;
  join(...paths: string[]): string;
  relative(from: string, to: string): string;
  resolve(...paths: string[]): string;
}

interface UrlModule {
  fileURLToPath(url: string): string;
}

const importNode = <T>(specifier: string): Promise<T> => import(/* @vite-ignore */ specifier) as Promise<T>;

async function loadNodeModules() {
  const [fs, path, url] = await Promise.all([
    importNode<FsModule>("node:fs"),
    importNode<PathModule>("node:path"),
    importNode<UrlModule>("node:url"),
  ]);
  const thisDir = path.dirname(url.fileURLToPath(import.meta.url));
  const srcRoot = path.resolve(thisDir, "..");
  return { fs, path, thisDir, srcRoot };
}

function walkFiles(fs: FsModule, path: PathModule, dir: string, exts: Set<string>, out: string[] = []): string[] {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      walkFiles(fs, path, full, exts, out);
    } else if (
      exts.has(path.extname(entry.name)) &&
      !entry.name.endsWith(".test.ts") &&
      !entry.name.endsWith(".test.tsx")
    ) {
      out.push(full);
    }
  }
  return out;
}

describe("Design tokens & zero undefined CSS custom properties (P3-3)", () => {
  it("defines --well and --wash in both light (:root) and dark (:root[data-theme='dark']) token scopes", async () => {
    const { fs, path, thisDir } = await loadNodeModules();
    const tokensCss = fs.readFileSync(path.join(thisDir, "tokens.css"), "utf8");
    const darkIdx = tokensCss.indexOf(':root[data-theme="dark"]');
    expect(darkIdx).toBeGreaterThan(0);

    const lightBlock = tokensCss.slice(0, darkIdx);
    const darkBlock = tokensCss.slice(darkIdx);

    expect(lightBlock).toMatch(/--well\s*:/);
    expect(lightBlock).toMatch(/--wash\s*:/);
    expect(darkBlock).toMatch(/--well\s*:/);
    expect(darkBlock).toMatch(/--wash\s*:/);
  });

  it("ensures 100% of var(--*) references across all .css and .tsx files in src/ are defined", async () => {
    const { fs, path, srcRoot } = await loadNodeModules();
    const definedVars = new Set<string>();

    const cssFiles = walkFiles(fs, path, srcRoot, new Set([".css"]));
    for (const file of cssFiles) {
      const content = fs.readFileSync(file, "utf8");
      for (const match of content.matchAll(/(--[a-zA-Z0-9_-]+)\s*:/g)) {
        definedVars.add(match[1]);
      }
    }

    const tsFiles = walkFiles(fs, path, srcRoot, new Set([".ts", ".tsx"]));
    for (const file of tsFiles) {
      const content = fs.readFileSync(file, "utf8");
      for (const match of content.matchAll(/["'](--[a-zA-Z0-9_-]+)["']\s*:/g)) {
        definedVars.add(match[1]);
      }
      for (const match of content.matchAll(/setProperty\(\s*["'](--[a-zA-Z0-9_-]+)["']/g)) {
        definedVars.add(match[1]);
      }
    }

    const scanTargets = [...cssFiles, ...walkFiles(fs, path, srcRoot, new Set([".tsx"]))];
    const dangling: { file: string; variable: string }[] = [];
    const forbiddenLegacy = new Set(["--border", "--paper", "--paper-2", "--gold-ink", "--hover"]);
    const legacyHits: { file: string; variable: string }[] = [];

    for (const file of scanTargets) {
      const content = fs.readFileSync(file, "utf8");
      const rel = path.relative(srcRoot, file);
      for (const match of content.matchAll(/var\(\s*(--[a-zA-Z0-9_-]+)/g)) {
        const variable = match[1];
        if (forbiddenLegacy.has(variable)) {
          legacyHits.push({ file: rel, variable });
        }
        if (!definedVars.has(variable)) {
          dangling.push({ file: rel, variable });
        }
      }
    }

    expect(legacyHits).toEqual([]);
    expect(dangling).toEqual([]);
  });
});
