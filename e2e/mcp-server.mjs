// A minimal MCP server over standard input and output, for the end-to-end
// test: one tool, `shout`, which returns its text in capitals. It speaks
// just enough of the protocol for a client to connect, list, and call.

import { createInterface } from "node:readline";

const reply = (id, result) => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, result })}\n`);

createInterface({ input: process.stdin }).on("line", (line) => {
  const message = JSON.parse(line);
  switch (message.method) {
    case "initialize":
      return reply(message.id, { protocolVersion: message.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: "e2e-notes", version: "1.0.0" } });
    case "tools/list":
      return reply(message.id, {
        tools: [{ name: "shout", description: "Returns the text in capitals.", inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] } }],
      });
    case "tools/call":
      return reply(message.id, { content: [{ type: "text", text: String(message.params.arguments.text).toUpperCase() }] });
    default:
      // Notifications need no answer.
      return undefined;
  }
});
