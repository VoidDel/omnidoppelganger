// The 法器 that come with the app: MCP servers in common use, each switched on by writing it into `mcp.json`
// and off by taking it out. None is on until the user switches it on.

/** One entry of `mcpServers`, as `crates/odg-core/src/mcp.rs` reads it. */
export interface ServerEntry {
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  url?: string;
  headers?: Record<string, string>;
  /** Switched off: kept in the list, not connected to. */
  disabled?: boolean;
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
  key?: { label: string; placeholder: string; help: string; /** The page where one is made. */ where?: string };
  /** Where to read about it. */
  site: string;
  /** Its entry, for a home folder (where it keeps what it keeps) and the key it was given. */
  entry: (home: string, key: string) => ServerEntry;
}

/** Derive the Magpie Search MCP URL (`<origin>/mcp/search`) from a Magpie provider's base URL. */
export function magpieSearchUrl(baseUrl?: string | null): string {
  const raw = (baseUrl ?? "").trim().replace(/\/+$/, "");
  if (!raw) return "http://127.0.0.1:3425/mcp/search";
  const origin = raw.endsWith("/v1") ? raw.slice(0, -3).replace(/\/+$/, "") : raw;
  return `${origin || "http://127.0.0.1:3425"}/mcp/search`;
}

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
    id: "magpie-search",
    name: "Magpie Search",
    seal: "鹊",
    summary: "接入已启用 Magpie 宗门的联网搜索与网页抓取（web_search / web_fetch），默认地址与 Magpie 宗门保持一致。",
    runs: "remote",
    site: "https://github.com/yetone/magpie",
    entry: (_, url) => ({ url: magpieSearchUrl(url) }),
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
      where: "https://github.com/settings/personal-access-tokens/new",
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

/** What a server may be named in the list: the characters the core accepts, which its tools are then known by. */
export const SERVER_NAME = /^[A-Za-z0-9_-]+$/;

/**
 * A command line taken apart as a terminal would: words split at spaces, a quoted stretch kept whole. A backslash
 * is kept as it is, for the Windows paths it is mostly found in.
 */
export function splitLine(line: string): string[] {
  const words: string[] = [];
  let word: string | null = null;
  let quote: string | null = null;
  for (const c of line) {
    if (quote) {
      if (c === quote) quote = null;
      else word += c;
    } else if (c === '"' || c === "'") {
      quote = c;
      word ??= "";
    } else if (/\s/.test(c)) {
      if (word !== null) words.push(word);
      word = null;
    } else {
      word = (word ?? "") + c;
    }
  }
  if (word !== null) words.push(word);
  return words;
}

/** Words put back into a command line that [`splitLine`] takes apart again into the same words. */
export function joinLine(words: string[]): string {
  return words.map((word) => (word === "" || /[\s"']/.test(word) ? (word.includes('"') ? `'${word}'` : `"${word}"`) : word)).join(" ");
}

/** Whether a value read from JSON looks like one server's entry: a program to start or an address to reach. */
const isEntry = (value: unknown): value is ServerEntry =>
  typeof value === "object" && value !== null && !Array.isArray(value) && (typeof (value as ServerEntry).command === "string" || typeof (value as ServerEntry).url === "string");

/**
 * What a piece of JSON pasted in gives: the servers it names (a whole `mcp.json`, or just its `mcpServers`, or
 * `{"name": {...}}`), or one server's entry with no name, or null for what is none of these.
 */
export function readPasted(text: string): { servers: Record<string, ServerEntry> } | { entry: ServerEntry } | null {
  let value: unknown;
  try {
    value = JSON.parse(text.trim());
  } catch {
    // A snippet copied out of a larger file often comes with the comma that followed it, or without its braces.
    try {
      value = JSON.parse(`{${text.trim().replace(/,\s*$/, "")}}`);
    } catch {
      return null;
    }
  }
  if (isEntry(value)) return { entry: value };
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const inner = (value as { mcpServers?: unknown }).mcpServers ?? value;
  if (typeof inner !== "object" || inner === null || Array.isArray(inner)) return null;
  const servers = Object.entries(inner as Record<string, unknown>).filter(([, entry]) => isEntry(entry)) as [string, ServerEntry][];
  return servers.length ? { servers: Object.fromEntries(servers) } : null;
}

/** One server as the add dialog's form holds it. */
export interface ServerForm {
  name: string;
  runs: "command" | "url";
  /** The command and its arguments, as typed at a terminal. */
  line: string;
  env: [string, string][];
  url: string;
  headers: [string, string][];
}

export const EMPTY_FORM: ServerForm = { name: "", runs: "command", line: "", env: [], url: "", headers: [] };

/** A server's entry as the form shows it. */
export function toForm(name: string, entry: ServerEntry): ServerForm {
  return {
    name,
    runs: entry.url !== undefined && entry.command === undefined ? "url" : "command",
    line: entry.command === undefined ? "" : joinLine([entry.command, ...(entry.args ?? [])]),
    env: Object.entries(entry.env ?? {}),
    url: entry.url ?? "",
    headers: Object.entries(entry.headers ?? {}),
  };
}

/** The entry the form gives, or what is missing from it, in the user's words. */
export function fromForm(form: ServerForm): ServerEntry | { error: string } {
  const pairs = (rows: [string, string][]) => Object.fromEntries(rows.map(([key, value]) => [key.trim(), value] as [string, string]).filter(([key]) => key));
  if (form.runs === "command") {
    const [command, ...args] = splitLine(form.line);
    if (!command) return { error: "填上要启动的命令，比如 npx -y @modelcontextprotocol/server-github" };
    const env = pairs(form.env);
    return { command, ...(args.length ? { args } : {}), ...(Object.keys(env).length ? { env } : {}) };
  }
  const url = form.url.trim();
  if (!/^https?:\/\/\S+$/.test(url)) return { error: "地址要以 http:// 或 https:// 开头" };
  const headers = pairs(form.headers);
  return { url, ...(Object.keys(headers).length ? { headers } : {}) };
}

/** Whether an entry will do for the list as the core reads it: a program to start or an http(s) address, not both. */
export function entryProblem(entry: ServerEntry): string | null {
  const command = typeof entry.command === "string" && entry.command.trim() !== "";
  const url = typeof entry.url === "string" && /^https?:\/\//.test(entry.url);
  return command === url ? "要么写 command（本机启动的程序），要么写 http(s) 开头的 url（远程服务），两者只能有一个" : null;
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
