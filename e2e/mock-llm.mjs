// A stand-in for the Anthropic Messages API: answers each request with the
// next scripted reply, streamed as server-sent events the way the real API
// streams them, and keeps every request body for the test to inspect.

import { createServer } from "node:http";

/**
 * @typedef {{ text: string } | { tool: string, input: object }} Block
 * @typedef {Block[]} Reply One assistant message: its blocks in order.
 */

/** The stream events of one reply, as the API sends them. */
function events(reply, index) {
  const sent = [
    { type: "message_start", message: { id: `msg_${index}`, model: "claude-opus-5-5", usage: { input_tokens: 100 * (index + 1), output_tokens: 1 } } },
  ];
  reply.forEach((block, at) => {
    if ("text" in block) {
      sent.push({ type: "content_block_start", index: at, content_block: { type: "text", text: "" } });
      // Two fragments, so the client's joining of fragments is exercised.
      const half = Math.ceil(block.text.length / 2);
      for (const text of [block.text.slice(0, half), block.text.slice(half)]) {
        if (text) sent.push({ type: "content_block_delta", index: at, delta: { type: "text_delta", text } });
      }
    } else {
      sent.push({ type: "content_block_start", index: at, content_block: { type: "tool_use", id: `toolu_${index}_${at}`, name: block.tool, input: {} } });
      const json = JSON.stringify(block.input);
      const half = Math.ceil(json.length / 2);
      for (const partial_json of [json.slice(0, half), json.slice(half)]) {
        sent.push({ type: "content_block_delta", index: at, delta: { type: "input_json_delta", partial_json } });
      }
    }
    sent.push({ type: "content_block_stop", index: at });
  });
  const stop_reason = reply.some((block) => "tool" in block) ? "tool_use" : "end_turn";
  sent.push({ type: "message_delta", delta: { stop_reason }, usage: { output_tokens: 20 } });
  sent.push({ type: "message_stop" });
  return sent;
}

/**
 * Start the server on a free port.
 * @param {Reply[]} script The replies, one per request, in order.
 * @returns {Promise<{ url: string, requests: object[], close: () => Promise<void> }>}
 */
export async function startMock(script) {
  const requests = [];
  const server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => (body += chunk));
    request.on("end", () => {
      if (request.method !== "POST" || request.url !== "/v1/messages") {
        response.writeHead(404, { "content-type": "application/json" }).end(JSON.stringify({ type: "error", error: { type: "not_found_error", message: request.url } }));
        return;
      }
      const index = requests.length;
      requests.push({ headers: request.headers, body: JSON.parse(body) });
      const reply = script[index];
      if (!reply) {
        // The app asked for more than the test scripted: fail loudly rather than hang.
        const error = { type: "error", error: { type: "invalid_request_error", message: `the mock has no reply for request ${index + 1}` } };
        response.writeHead(400, { "content-type": "application/json" }).end(JSON.stringify(error));
        return;
      }
      response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      for (const event of events(reply, index)) response.write(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`);
      response.end();
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return {
    url: `http://127.0.0.1:${server.address().port}`,
    requests,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}
