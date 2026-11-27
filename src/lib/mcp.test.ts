import { describe, expect, it } from "vitest";
import { BUILTINS, EMPTY_FORM, entryProblem, fromForm, joinLine, launchLine, magpieSearchUrl, parseServers, readPasted, splitLine, toForm, withServer } from "./mcp";

describe("a 法器 added by hand", () => {
  it("takes its command line apart as a terminal would, keeping Windows paths and quoted words whole", () => {
    expect(splitLine("npx -y @modelcontextprotocol/server-github")).toEqual(["npx", "-y", "@modelcontextprotocol/server-github"]);
    expect(splitLine('  node "C:\\My Tools\\server.js"  --name \'a b\' ""')).toEqual(["node", "C:\\My Tools\\server.js", "--name", "a b", ""]);
    for (const words of [
      ["node", "C:\\My Tools\\s.js", "--x", ""],
      ["echo", 'say "hi"', "it's"],
    ]) {
      expect(splitLine(joinLine(words))).toEqual(words);
    }
  });

  it("reads what is pasted: a whole list, its servers, one named server, or one entry", () => {
    const entry = { command: "npx", args: ["-y", "x"] };
    expect(readPasted(JSON.stringify({ mcpServers: { x: entry } }))).toEqual({ servers: { x: entry } });
    expect(readPasted(JSON.stringify({ x: entry, y: { url: "https://y" } }))).toEqual({ servers: { x: entry, y: { url: "https://y" } } });
    expect(readPasted(JSON.stringify(entry))).toEqual({ entry });
    // A snippet cut out of a bigger file, without its braces and with a trailing comma.
    expect(readPasted('"x": {"command": "npx", "args": ["-y", "x"]},')).toEqual({ servers: { x: entry } });
    expect(readPasted("npx -y x")).toBeNull();
    expect(readPasted('{"name": "x"}')).toBeNull();
  });

  it("goes from the form to an entry and back", () => {
    const entry = { command: "node", args: ["C:\\My Tools\\s.js"], env: { TOKEN: "${TOKEN}" } };
    const form = toForm("tools", entry);
    expect(form).toMatchObject({ name: "tools", runs: "command", line: 'node "C:\\My Tools\\s.js"' });
    expect(fromForm(form)).toEqual(entry);
    const remote = fromForm({ ...EMPTY_FORM, runs: "url", url: "https://x/mcp", headers: [["Authorization", "Bearer k"], ["  ", "dropped"]] });
    expect(remote).toEqual({ url: "https://x/mcp", headers: { Authorization: "Bearer k" } });
    expect(fromForm({ ...EMPTY_FORM, line: "  " })).toHaveProperty("error");
    expect(fromForm({ ...EMPTY_FORM, runs: "url", url: "mcp.example.com" })).toHaveProperty("error");
    expect(entryProblem({ command: "x", url: "https://x" })).not.toBeNull();
    expect(entryProblem({ url: "https://x" })).toBeNull();
  });
});

describe("the 法器 that come with the app", () => {
  it("are switched on by writing one into the list, and off by taking it out, leaving the rest as it was", () => {
    const custom = JSON.stringify({ note: "mine", mcpServers: { local: { command: "my-server", args: ["--fast"] } } });
    const on = withServer(custom, "context7", { url: "https://mcp.context7.com/mcp" });
    if (typeof on !== "string") throw new Error(on.error);
    expect(JSON.parse(on)).toEqual({ note: "mine", mcpServers: { local: { command: "my-server", args: ["--fast"] }, context7: { url: "https://mcp.context7.com/mcp" } } });
    const off = withServer(on, "context7", null);
    if (typeof off !== "string") throw new Error(off.error);
    expect(JSON.parse(off)).toEqual(JSON.parse(custom));
    // An empty file is an empty list.
    expect(parseServers("")).toEqual({});
    expect(JSON.parse(withServer("", "exa", { url: "https://mcp.exa.ai/mcp" }) as string)).toEqual({ mcpServers: { exa: { url: "https://mcp.exa.ai/mcp" } } });
  });

  it("do not write over a list that cannot be read", () => {
    expect(withServer("{ not json", "exa", { url: "x" })).toEqual({ error: expect.stringContaining("不是有效的 JSON") });
    expect(withServer('{"mcpServers": []}', "exa", { url: "x" })).toEqual({ error: "mcpServers 应当是一个 JSON 对象" });
  });

  it("are each named as a server may be, and keep what they keep in the home folder", () => {
    expect(BUILTINS.every((builtin) => /^[A-Za-z0-9_-]+$/.test(builtin.id))).toBe(true);
    expect(new Set(BUILTINS.map((builtin) => builtin.id)).size).toBe(BUILTINS.length);
    const memory = BUILTINS.find((builtin) => builtin.id === "memory")!;
    expect(memory.entry("C:\\Users\\me\\.odg", "").env?.MEMORY_FILE_PATH).toBe("C:\\Users\\me\\.odg\\mcp-memory.jsonl");
    expect(memory.entry("/home/me/.odg/", "").env?.MEMORY_FILE_PATH).toBe("/home/me/.odg/mcp-memory.jsonl");
    const github = BUILTINS.find((builtin) => builtin.id === "github")!;
    expect(github.entry("", "ghp_x").headers).toEqual({ Authorization: "Bearer ghp_x" });
    // Each is either a program to start or an address to reach, as the core requires.
    expect(BUILTINS.every((builtin) => !!builtin.entry("/h", "k").command !== !!builtin.entry("/h", "k").url)).toBe(true);
  });

  it("show how each is started or reached", () => {
    expect(launchLine({ command: "uvx", args: ["mcp-server-fetch"] })).toBe("uvx mcp-server-fetch");
    expect(launchLine({ url: "https://mcp.deepwiki.com/mcp", headers: { Authorization: "secret" } })).toBe("https://mcp.deepwiki.com/mcp");
  });

  it("derives the Magpie Search MCP URL from a Magpie provider base URL", () => {
    expect(magpieSearchUrl("")).toBe("http://127.0.0.1:3425/mcp/search");
    expect(magpieSearchUrl("http://127.0.0.1:3425/v1")).toBe("http://127.0.0.1:3425/mcp/search");
    expect(magpieSearchUrl("http://192.168.1.50:3425/v1/")).toBe("http://192.168.1.50:3425/mcp/search");
    expect(magpieSearchUrl("https://magpie.example.com/v1")).toBe("https://magpie.example.com/mcp/search");
    expect(magpieSearchUrl("http://10.0.0.8:3425")).toBe("http://10.0.0.8:3425/mcp/search");
  });
});
