import { describe, expect, it } from "vitest";
import { BUILTINS, launchLine, parseServers, withServer } from "./mcp";

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
});
