// End-to-end test: the real app window, driven through WebView2's debugging
// port, talking to a scripted stand-in for the model.
//
//   node e2e/run.mjs --build   build the app into target/e2e, then run
//   node e2e/run.mjs           run against the last build
//
// Windows only: the debugging port is WebView2's. The app runs with its own
// home, workspace, and browser profile in a temporary directory, so it
// touches neither the user's sessions and keys nor a copy of the app that is
// already open.

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { connect } from "./cdp.mjs";
import { startMock } from "./mock-llm.mjs";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
// Its own target directory: a running `tauri dev` holds the usual one's executable open.
const target = join(root, "target", "e2e");
const executable = join(target, "debug", "omnidoppelganger.exe");
const DEBUG_PORT = 9333;
/** A one-pixel PNG, standing in for a pasted screenshot. */
const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

if (process.platform !== "win32") {
  console.error("The end-to-end test drives WebView2 and runs on Windows only.");
  process.exit(1);
}
if (process.argv.includes("--build")) {
  // A build with the frontend inside it; a plain `cargo build` loads the dev server instead.
  // pnpm is a script on Windows, so it runs through the shell, as one fixed command line.
  const built = spawnSync("pnpm tauri build --debug --no-bundle", { cwd: root, stdio: "inherit", shell: true, env: { ...process.env, CARGO_TARGET_DIR: target } });
  if (built.status !== 0) process.exit(built.status ?? 1);
}
if (!existsSync(executable)) {
  console.error(`${executable} is missing: run with --build first.`);
  process.exit(1);
}

let failed = false;
const step = (text) => console.log(`  ✓ ${text}`);
const check = (condition, what) => {
  if (!condition) throw new Error(`expected ${what}`);
  step(what);
};

const scratch = mkdtempSync(join(tmpdir(), "odg-e2e-"));
const home = join(scratch, "home");
const workspace = join(scratch, "workspace");
mkdirSync(workspace);
// An MCP server for the app to start as a process and speak to over its standard streams.
mkdirSync(home);
writeFileSync(join(home, "mcp.json"), JSON.stringify({ mcpServers: { notes: { command: "node", args: [join(root, "e2e", "mcp-server.mjs")] } } }));
const hello = join(workspace, "hello.txt");

// What the stand-in model says: create a file and run a command, then wrap up.
const mock = await startMock([
  [
    { text: "我先建文件，再跑命令。" },
    { tool: "update_todos", input: { todos: [{ text: "建文件", status: "in_progress" }, { text: "跑命令", status: "pending" }] } },
    { tool: "edit_file", input: { path: "hello.txt", old_string: "", new_string: "你好，分身\n" } },
    { tool: "mcp__notes__shout", input: { text: "hello from the server" } },
    { tool: "run_command", input: { command: "echo e2e-ok" } },
  ],
  [{ text: "都办好了。\n\n| 指标 | 结果 |\n|---|---|\n| 通过 | 119 |\n\n```rust\nfn main() {}\n```\n\n```nosuchlanguage\nplain words\n```" }],
  // In a second session: the model hands a task to a 子分身, whose one request comes while it waits.
  [{ text: "我派一个子分身去查。" }, { tool: "delegate", input: { tasks: [{ name: "查文件", task: "看看 hello.txt 还在不在" }] } }],
  [{ text: "子分身查过了：不在了。" }],
  [{ text: "都查完了。" }],
]);

const app = spawn(executable, [], {
  stdio: "ignore",
  env: {
    ...process.env,
    ODG_HOME: home,
    ODG_E2E_WORKSPACE: workspace,
    ANTHROPIC_BASE_URL: mock.url,
    ANTHROPIC_API_KEY: "e2e-key",
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${DEBUG_PORT}`,
    WEBVIEW2_USER_DATA_FOLDER: join(scratch, "webview"),
  },
});

let page;
try {
  console.log("A turn with a file edit, a command that asks, and an undo");
  page = await connect(DEBUG_PORT);
  // The app starts the MCP server in the background; its tool is offered to the model once it is connected.
  // (Asked through the app's own bridge to its backend, which the page gets a moment after it opens.)
  const connected = `window.__TAURI_INTERNALS__?.invoke("get_settings").then((view) => view.mcp_servers[0]?.state === "ready" && view.mcp_servers[0].tools[0] === "shout")`;
  await page.waitFor("the MCP server to connect", connected);
  step("the app started the MCP server from mcp.json and listed its tool");
  await page.click("选文件夹作洞府，化出分身");
  await page.waitFor("the message box", `!!document.querySelector("#composer-input")`);

  // The model menu lists what the provider says it serves; picking one moves the session to it.
  const chip = (label) => `[...document.querySelectorAll(".composer-row .select-button")].find((b) => b.querySelector(".label")?.textContent === ${JSON.stringify(label)})?.querySelector(".value")?.textContent`;
  await page.click("功法");
  await page.waitForText("它报了 2 门");
  await page.click("claude-e2e-small");
  await page.waitFor("the session to move to the picked model", `${chip("功法")} === "claude-e2e-small"`);
  step("the model menu listed the provider's models, and picking one moved the session to it");

  // The 灵石 menu sets the context limit, which is saved as a setting.
  await page.click("灵石");
  await page.waitForText("前情上限");
  await page.click("5 万");
  await page.waitFor("the limit to change", `${chip("灵石")}?.endsWith("/ 5 万")`);
  check(JSON.parse(readFileSync(join(home, "settings.json"), "utf8")).context_limit === 50000, "the 灵石 menu changed the context limit and saved it");

  // The side panel widens when its left edge is dragged, and can be put away and brought back as it was.
  const panelWidth = `document.querySelector(".panel")?.getBoundingClientRect().width`;
  const narrow = await page.evaluate(panelWidth);
  const edge = await page.evaluate(`(() => { const box = document.querySelector(".panel-edge").getBoundingClientRect(); return { x: box.left + box.width / 2, y: box.top + 200 }; })()`);
  await page.drag(edge, { x: edge.x - 120, y: edge.y });
  await page.waitFor("the panel to widen by the distance dragged", `Math.abs(${panelWidth} - ${narrow + 120}) <= 2`);
  await page.click("收起侧栏");
  await page.waitFor("the panel to be put away", `!document.querySelector(".panel")`);
  await page.click("展开侧栏");
  await page.waitFor("the panel to come back as wide as it was left", `Math.abs(${panelWidth} - ${narrow + 120}) <= 2`);
  // (Within two pixels: the width is kept as a whole number, and the screen's scaling puts the pointer between pixels.)
  const kept = JSON.parse(await page.evaluate(`localStorage.getItem("odg.panel")`));
  check(Math.abs(kept.width - (narrow + 120)) <= 2 && kept.hidden === false, "the side panel resizes by dragging, hides, and comes back, and its width is remembered");

  // Paste a screenshot the way the system clipboard delivers one: a file on a paste event.
  await page.evaluate(`(() => {
    const bytes = Uint8Array.from(atob(${JSON.stringify(PNG)}), (c) => c.charCodeAt(0));
    const data = new DataTransfer();
    data.items.add(new File([bytes], "shot.png", { type: "image/png" }));
    document.querySelector("#composer-input").dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
  })()`);
  await page.waitFor("the pasted image's thumbnail", `document.querySelectorAll(".attachment img").length === 1`);
  await page.type("#composer-input", "建一个 hello.txt，再跑个命令");
  await page.enter();
  step("a session opens in the workspace and takes a message with a pasted screenshot");

  // The edit runs on its own in the default mode; the command waits for the user.
  // The server's tool asks first, whatever the mode; then the command does.
  await page.waitForText("外来的法器");
  await page.waitForText("notes · shout");
  await page.click("准这一次");
  await page.waitForText("分身要运行一条命令");
  check(readFileSync(hello, "utf8") === "你好，分身\n", "the edit wrote hello.txt while the command still waited for approval");
  await page.click("准这一次");
  await page.waitForText("都办好了。");
  await page.waitFor("the turn to end", `!document.body.innerText.includes("分身正在运功")`);
  step("the approved command ran and the model's closing words are shown");

  const [first, second] = mock.requests.map((request) => request.body);
  check(mock.requests.length === 2 && mock.requests[0].headers["x-api-key"] === "e2e-key", "the model was asked twice, with the key");
  check(first.messages[0].content.some((block) => block.text === "建一个 hello.txt，再跑个命令") && first.model === "claude-e2e-small", "the first request carries the user's message, to the model that was picked");
  const sent = first.messages[0].content[0];
  check(sent.type === "image" && sent.source.media_type === "image/png" && sent.source.data === PNG, "the pasted screenshot reached the model unchanged, ahead of the text");
  check((await page.evaluate(`document.querySelectorAll(".shots img").length`)) === 1, "the transcript shows the screenshot with the message");
  const results = second.messages[second.messages.length - 1].content.filter((block) => block.type === "tool_result");
  check(results.length === 4 && results.every((result) => !result.is_error), "the second request carries all four tool results");
  check(first.tools.some((tool) => tool.name === "mcp__notes__shout") && results[2].content === "HELLO FROM THE SERVER", "the model was offered the server's tool, and its call went to the server and back");
  check(results[3].content.includes("e2e-ok") && !results[3].content.includes("\u001b"), "the command's output reached the model without color codes");

  // The model's code is colored by language, each block can be copied, and an unknown language stays plain.
  const code = await page.evaluate(`({
    keyword: document.querySelector(".prose .hljs-keyword")?.textContent,
    buttons: document.querySelectorAll(".code-block .copy").length,
    plain: document.body.innerText.includes("plain words"),
  })`);
  check(code.keyword === "fn" && code.buttons === 2 && code.plain, "code blocks are highlighted and have copy buttons");
  check((await page.evaluate(`[...document.querySelectorAll(".prose table th, .prose table td")].map((cell) => cell.textContent).join()`)) === "指标,结果,通过,119", "a table the model writes is shown as a table");
  await page.click("计划 0/2");
  await page.waitForText("分身的计划");
  check(await page.evaluate(`[...document.querySelectorAll(".todo")].map((item) => item.className).join()`) === "todo in_progress,todo pending", "the plan tab shows the to-do list");
  await page.click("改动");

  await page.waitForText("hello.txt");
  await page.click("撤回这一轮起的改动");
  await page.click("确认撤回");
  await page.waitForText("本尊撤回了第 1 轮起的改动");
  check(!existsSync(hello), "undoing the turn removed the file the agent created");

  const [session] = readdirSync(join(home, "sessions"));
  const log = readFileSync(join(home, "sessions", session, "session.v1.jsonl"), "utf8").trim().split("\n").map((line) => JSON.parse(line));
  const kinds = log.map((event) => event.type);
  check(kinds[0] === "session/created" && kinds.includes("approval/resolved") && kinds.at(-1) === "files/restored", "the session log holds the turn, the approval, and the undo");
  check(log.find((event) => event.type === "turn/end").data.reason === "completed", "the turn ended as completed");

  // The + beside a workspace's name starts another session there, with no folder to pick again.
  await page.evaluate(`document.querySelector(".group-add").click()`);
  await page.waitFor("a second session in the same workspace", `document.querySelectorAll(".group .row-button").length === 2 && document.querySelectorAll(".group").length === 1`);
  check(readdirSync(join(home, "sessions")).length === 2 && (await page.evaluate(`document.querySelector(".main-header h1").textContent`)) === "新分身", "the + beside a workspace opened a new session in it");
  // There the 分身 sets off a 子分身: a session of its own, listed under this one, with a way in and a way back.
  await page.type("#composer-input", "分头去查");
  await page.enter();
  await page.waitForText("都查完了。");
  await page.waitFor("the turn to end", `!document.body.innerText.includes("分身正在运功")`);
  const card = await page.evaluate(`({
    state: document.querySelector(".did.delegate .agent .chip")?.textContent,
    name: document.querySelector(".did.delegate .agent-name")?.textContent,
    listed: [...document.querySelectorAll(".sidebar .row-button.child")].map((row) => row.textContent),
  })`);
  check(card.state === "已回报" && card.name === "查文件" && card.listed.join() === "查文件", "the 子分身 is shown in the call that set it off, and under its 分身 in the list");
  const [, , asked, sub, told] = mock.requests.map((request) => request.body);
  const offered = (body) => body.tools.some((tool) => tool.name === "delegate");
  check(offered(asked) && !offered(sub) && JSON.stringify(sub.system).includes("You are a subagent") && sub.messages[0].content[0].text === "看看 hello.txt 还在不在", "the 子分身 got the task and nothing else, and cannot set off its own");
  const report = told.messages[told.messages.length - 1].content.find((block) => block.type === "tool_result");
  check(report.content.includes("子分身查过了：不在了。") && !report.is_error && readdirSync(join(home, "sessions")).length === 3, "its report went back to the 分身, and its session is kept");
  const title = `document.querySelector(".main-header h1")?.textContent`;
  await page.evaluate(`document.querySelector(".did.delegate .agent").click()`);
  await page.waitFor("the 子分身's own page", `${title} === "查文件" && !!document.querySelector(".kin-link")`);
  await page.waitForText("看看 hello.txt 还在不在");
  await page.waitForText("主分身交办");
  await page.evaluate(`document.querySelector(".kin-link").click()`);
  await page.waitFor("the way back to the 分身", `${title} === "分头去查"`);
  step("the 子分身's page shows its task and work, and leads back to the 分身 that set it off");
  console.log("Passed.");
} catch (error) {
  failed = true;
  console.error(`  ✗ ${error.message}`);
  if (page) console.error(`\nThe window showed:\n${await page.evaluate("document.body.innerText").catch(() => "(could not read the page)")}`);
} finally {
  page?.close();
  // The window's WebView2 processes are its children: take the whole tree.
  spawnSync("taskkill", ["/PID", String(app.pid), "/T", "/F"], { stdio: "ignore" });
  await mock.close();
  rmSync(scratch, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
process.exit(failed ? 1 : 0);
