// The 法器 that come with the app: MCP servers in common use, each switched on by writing it into `mcp.json`
// and off by taking it out. None is on until the user switches it on.

/** One entry of `mcpServers`, as `crates/odg-core/src/mcp.rs` reads it. */
export interface ServerEntry {
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  url?: string;
  headers?: Record<string, string>;
}

export interface Builtin {
  /** Its name in `mcpServers`, which its tools are known by. */
  id: string;
  name: string;
  /** The character on its seal. */
  seal: string;
  /** What it lets the 分身 do. */
  summary: string;
  /** Reached over the network, or started on this computer. */
  runs: "remote" | "local";
  /** What has to be installed for it to start: Node.js for `npx`, uv for `uvx`. */
  needs?: "node" | "uv";
  /** A secret it needs, asked for when it is switched on. */
  key?: { label: string; placeholder: string; help: string };
  /** Where to read about it. */
  site: string;
  /** Its entry, for a home folder (where it keeps what it keeps) and the key it was given. */
  entry: (home: string, key: string) => ServerEntry;
}

/** A path in the home folder, with the separator the folder is written with. */
const inHome = (home: string, name: string) => `${home.replace(/[\\/]+$/, "")}${home.includes("\\") ? "\\" : "/"}${name}`;

export const BUILTINS: Builtin[] = [
  {
    id: "context7",
    name: "Context7",
    seal: "典",
    summary: "查各种库和框架当前版本的文档和示例，免得分身照着过时的写法写代码。",
    runs: "remote",
    site: "https://context7.com",
    entry: () => ({ url: "https://mcp.context7.com/mcp" }),
  },
  {
    id: "deepwiki",
    name: "DeepWiki",
    seal: "问",
    summary: "读懂 GitHub 上任一公开仓库：问它的结构、模块和用法，不用先把它克隆下来。",
    runs: "remote",
    site: "https://deepwiki.com",
    entry: () => ({ url: "https://mcp.deepwiki.com/mcp" }),
  },
  {
    id: "microsoft-learn",
    name: "Microsoft Learn",
    seal: "微",
    summary: "查微软的官方文档：.NET、Azure、Windows、PowerShell 等，带代码示例。",
    runs: "remote",
    site: "https://learn.microsoft.com",
    entry: () => ({ url: "https://learn.microsoft.com/api/mcp" }),
  },
  {
    id: "exa",
    name: "Exa",
    seal: "搜",
    summary: "联网搜索网页和代码。不用存 GLM 的密钥，修别家心法的分身也能搜。",
    runs: "remote",
    site: "https://exa.ai",
    entry: () => ({ url: "https://mcp.exa.ai/mcp" }),
  },
  {
    id: "github",
    name: "GitHub",
    seal: "仓",
    summary: "读写 GitHub 上的 issue、合并请求、代码和 Actions 的运行结果。",
    runs: "remote",
    key: {
      label: "GitHub 个人访问令牌",
      placeholder: "github_pat_… 或 ghp_…",
      help: "在 GitHub 的 Settings → Developer settings → Personal access tokens 里生成，只给它要用的仓库和权限。令牌和清单一起存在 mcp.json 里，是明文。",
    },
    site: "https://github.com/github/github-mcp-server",
    entry: (_, key) => ({ url: "https://api.githubcopilot.com/mcp/", headers: { Authorization: `Bearer ${key}` } }),
  },
  {
    id: "playwright",
    name: "Playwright",
    seal: "驭",
    summary: "让分身操控浏览器：打开网页、点击、填表、截图，适合验证它改过的网页。",
    runs: "local",
    needs: "node",
    site: "https://github.com/microsoft/playwright-mcp",
    entry: () => ({ command: "npx", args: ["-y", "@playwright/mcp@latest"] }),
  },
  {
    id: "chrome-devtools",
    name: "Chrome DevTools",
    seal: "察",
    summary: "用 Chrome 的开发者工具调试网页：看控制台、网络请求和性能。要装有 Chrome。",
    runs: "local",
    needs: "node",
    site: "https://github.com/ChromeDevTools/chrome-devtools-mcp",
    entry: () => ({ command: "npx", args: ["-y", "chrome-devtools-mcp@latest"] }),
  },
  {
    id: "sequential-thinking",
    name: "Sequential Thinking",
    seal: "思",
    summary: "给分身一个分步思考的草稿本，把复杂的问题拆开，一步一步推，推错了能回头改。",
    runs: "local",
    needs: "node",
    site: "https://github.com/modelcontextprotocol/servers/tree/main/src/sequentialthinking",
    entry: () => ({ command: "npx", args: ["-y", "@modelcontextprotocol/server-sequential-thinking"] }),
  },
  {
    id: "memory",
    name: "Memory",
    seal: "忆",
    summary: "给分身一份跨会话的记忆：人、事、项目和它们之间的关系，存在 odg 的家目录里，换了分身也记得。",
    runs: "local",
    needs: "node",
    site: "https://github.com/modelcontextprotocol/servers/tree/main/src/memory",
    entry: (home) => ({ command: "npx", args: ["-y", "@modelcontextprotocol/server-memory"], env: { MEMORY_FILE_PATH: inHome(home, "mcp-memory.jsonl") } }),
  },
  {
    id: "fetch",
    name: "Fetch",
    seal: "取",
    summary: "取网页并转成 Markdown。分身自带的 fetch_url 已经能取网页，这是另一种取法。",
    runs: "local",
    needs: "uv",
    site: "https://github.com/modelcontextprotocol/servers/tree/main/src/fetch",
    entry: () => ({ command: "uvx", args: ["mcp-server-fetch"] }),
  },
];

/** What has to be installed first, in the user's words. */
export const NEEDS: Record<NonNullable<Builtin["needs"]>, string> = { node: "需要 Node.js", uv: "需要 uv（Python）" };

/** How an entry is started or reached, as a line under its name: the command, or the address. */
export function launchLine(entry: ServerEntry): string {
  return entry.url ?? [entry.command, ...(entry.args ?? [])].join(" ");
}

/** The servers `mcp.json`'s text lists, by name, or what is wrong with the text. */
export function parseServers(text: string): Record<string, ServerEntry> | { error: string } {
  if (!text.trim()) return {};
  try {
    const file = JSON.parse(text);
    if (typeof file !== "object" || file === null || Array.isArray(file)) return { error: "清单应当是一个 JSON 对象" };
    const servers = file.mcpServers ?? {};
    if (typeof servers !== "object" || servers === null || Array.isArray(servers)) return { error: "mcpServers 应当是一个 JSON 对象" };
    return servers as Record<string, ServerEntry>;
  } catch (error) {
    return { error: `不是有效的 JSON：${error instanceof Error ? error.message : String(error)}` };
  }
}

/**
 * `mcp.json`'s text with one server put in (or replaced) under `id`, or taken out when `entry` is null. The rest
 * of the file stays as it was, though it is written out again in the same shape; a file that cannot be read is
 * not written over.
 */
export function withServer(text: string, id: string, entry: ServerEntry | null): string | { error: string } {
  const servers = parseServers(text);
  if ("error" in servers && typeof servers.error === "string") return { error: servers.error };
  const file = text.trim() ? JSON.parse(text) : {};
  const next: Record<string, ServerEntry> = { ...(servers as Record<string, ServerEntry>) };
  if (entry) next[id] = entry;
  else delete next[id];
  return `${JSON.stringify({ ...file, mcpServers: next }, null, 2)}\n`;
}
