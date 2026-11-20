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
// The workspace is a Git repository with one commit, so a session can be given a worktree of its own.
const git = (...args) => spawnSync("git", ["-C", workspace, "-c", "user.name=e2e", "-c", "user.email=e2e@localhost", ...args], { encoding: "utf8" });
writeFileSync(join(workspace, "seed.txt"), "seed\n");
for (const args of [["init", "--quiet", "--initial-branch", "main"], ["add", "-A"], ["commit", "--quiet", "-m", "seed"]]) {
  const ran = git(...args);
  if (ran.status !== 0) {
    console.error(`git ${args[0]} failed in the test's workspace: ${ran.stderr || ran.error}`);
    process.exit(1);
  }
}

/** A one-page PDF of these lines in Helvetica, as small as a PDF can be and still be read for its words. */
function pdfOf(lines) {
  const words = lines.map((line, row) => `BT /F1 12 Tf 72 ${760 - 16 * row} Td (${line}) Tj ET\n`).join("");
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    `<< /Length ${words.length} >>\nstream\n${words}endstream`,
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
  ];
  let pdf = "%PDF-1.4\n";
  const offsets = objects.map((object, at) => {
    const offset = pdf.length;
    pdf += `${at + 1} 0 obj\n${object}\nendobj\n`;
    return offset;
  });
  const xref = pdf.length;
  pdf += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n${offsets.map((offset) => `${String(offset).padStart(10, "0")} 00000 n \n`).join("")}`;
  return pdf + `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
}

// Files to attach, outside the workspace: a small one, one too large to send with a message, one that is not text, and a PDF.
const attachedNotes = join(scratch, "要点.md");
const attachedLog = join(scratch, "build.log");
const attachedBinary = join(scratch, "archive.zip");
const attachedPdf = join(scratch, "sheet.pdf");
writeFileSync(attachedNotes, "# 要点\n先读配置\n");
writeFileSync(attachedLog, "first line of the log\n" + "x".repeat(120_000) + "\n");
writeFileSync(attachedBinary, Buffer.from([80, 75, 3, 4, 0, 0, 0, 0]));
writeFileSync(attachedPdf, pdfOf(["Pin 1 is VIN", "Pin 2 is GND"]));

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
  // In a third session, with a worktree of its own: it makes a file there.
  [{ text: "我在这边建个文件。" }, { tool: "edit_file", input: { path: "apart.txt", old_string: "", new_string: "来自独立工作树\n" } }],
  [{ text: "建好了。" }],
  // Two sessions that 功课 (schedules) start: one by hand, one when its time comes.
  [{ text: "清点完了。" }],
  [{ text: "报到。" }],
  // In a fork of the second session: the conversation it copied, and one more message.
  [{ text: "分叉这边查完了。" }],
  // Asked for by hand from the 灵石 menu: a summary of the conversation.
  [{ text: "<summary>本尊让分身分头去查，子分身查过了。</summary>" }],
  // A long thought, held while the test looks at it, then the answer.
  [
    { thinking: "1. Read the README and the roadmap 2. List the crates and what each owns 3. Run the tests 4. Check the CI config 5. Compare what the docs claim with the code 6. Look at open issues 7. Code drift, docs against reality 8. Maybe check dependency versions and vulnerabilities 9. Look at code smells, large files, complexity", hold: 2500 },
    { text: "推演完了。" },
  ],
  // Left to ask about nothing, in a worktree of its own: a command that always asks runs at once.
  [{ text: "托付了，我直接写。" }, { tool: "run_command", input: { command: "echo trusted > trusted.txt" } }],
  [{ text: "写好了。" }],
  // In 放手施为 with a 护法: the first command it lets run; the second it leaves to 本尊, who is not there.
  [{ text: "我写两个文件。" }, { tool: "run_command", input: { command: "echo hi > guarded.txt" } }],
  [{ text: "ALLOW\n只在洞府里写一个文件。" }],
  [{ tool: "run_command", input: { command: "echo again > second.txt" } }],
  [{ text: "ASK\n这一条先问问本尊。" }],
  [{ text: "第二个没写成，等本尊点头。" }],
  // With files attached: it reads the large one, which lies outside the workspace, by itself.
  [{ text: "我看看那份大的。" }, { tool: "read_file", input: { path: attachedLog, limit: 2 } }],
  [{ text: "两份附件都看过了。" }],
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
  await page.click("心法");
  await page.waitForText("它报了 2 门");
  await page.click("claude-e2e-small");
  await page.waitFor("the session to move to the picked model", `${chip("心法")} === "claude-e2e-small"`);
  step("the model menu listed the provider's models, and picking one moved the session to it");

  // The 境界 menu sets how hard this 分身 works, apart from the settings' default.
  await page.click("境界");
  await page.click("练气");
  await page.waitFor("the session to work at the realm picked", `${chip("境界")} === "练气"`);
  step("the 境界 menu set this 分身's realm");

  // The 灵石 menu sets the context limit, which is saved as a setting.
  await page.click("灵石");
  await page.waitForText("前情上限");
  await page.click("50K");
  await page.waitFor("the limit to change", `${chip("灵石")}?.endsWith("/ 50K")`);
  check(JSON.parse(readFileSync(join(home, "settings.json"), "utf8")).context_limit === 50000, "the 灵石 menu changed the context limit and saved it");

  // The side panel widens when its left edge is dragged, and can be put away and brought back as it was.
  const panelWidth = `document.querySelector(".panel")?.getBoundingClientRect().width`;
  // With nothing in it yet, it stands as a strip; a tab pressed there opens it, and it widens to its width.
  check(await page.evaluate(`!!document.querySelector(".panel.strip") && ${panelWidth} < 80`), "the side panel stands as a narrow strip while it holds nothing");
  await page.click("改动");
  const settled = `Math.abs(${panelWidth} - parseFloat(getComputedStyle(document.querySelector(".app")).getPropertyValue("--panel-width"))) <= 1`;
  await page.waitFor("the panel to open to its width", `!document.querySelector(".panel.strip") && ${panelWidth} > 200 && ${settled}`);
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
  await page.waitForText("身外法器");
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
  check(first.output_config?.effort === "low" && JSON.parse(readFileSync(join(home, "settings.json"), "utf8")).effort !== "low", "it is asked at the realm set for this 分身, which leaves the default as it was");
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
  await page.evaluate(`document.querySelector(".group-add:not(.apart)").click()`);
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
  // A 分身 with a worktree of its own works on a branch, apart from the folder's files, until its work is brought back.
  await page.evaluate(`document.querySelector(".group-add.apart").click()`);
  await page.waitFor("a session on a branch of its own", `document.querySelector(".main-header .branch")?.textContent.trim().startsWith("odg/")`);
  await page.type("#composer-input", "建个文件");
  await page.enter();
  await page.waitForText("建好了。");
  await page.waitFor("the turn to end", `!document.body.innerText.includes("分身正在运功")`);
  const apart = join(workspace, "apart.txt");
  const [checkout] = readdirSync(join(home, "worktrees"));
  check(!existsSync(apart) && existsSync(join(home, "worktrees", checkout, "apart.txt")) && (await page.evaluate(`document.querySelectorAll(".group").length`)) === 1, "its edit went into its own worktree, and it is listed with the folder it stands for");
  await page.click("并回");
  await page.waitForText("已并回");
  check(readFileSync(apart, "utf8").replace(/\r\n/g, "\n") === "来自独立工作树\n" && git("log", "--format=%s").stdout.includes("odg: 建个文件"), "bringing its work back committed it and merged it into the folder's branch");
  await page.click("删除");
  await page.click("确认删除");
  await page.waitFor("the worktree to go with its session", `!document.querySelector(".main-header .branch")`);
  check(readdirSync(join(home, "worktrees")).length === 0 && git("branch", "--list", "odg/*").stdout.trim() === "", "deleting the session removed its worktree and its merged branch");
  // 功课: a task set to start by itself. The form sets one; "现在就办" starts it without waiting for its time.
  await page.click("功课");
  await page.waitForText("还没有功课。");
  await page.click("添一门功课");
  await page.waitFor("the form, with the folder of the sessions there are", `document.querySelector("#schedule-folder")?.value === ${JSON.stringify(workspace)}`);
  await page.type("#schedule-prompt", "清点洞府里的文件");
  await page.type("#schedule-name", "晨课");
  await page.click("定下这门功课");
  await page.waitForText("每天 09:00");
  const schedules = () => JSON.parse(readFileSync(join(home, "schedules.json"), "utf8"));
  check(schedules().length === 1 && schedules()[0].name === "晨课" && schedules()[0].workspace === workspace && Date.parse(schedules()[0].next) > Date.now(), "the form set a 功课 for nine every morning, kept with the time it is next due");
  await page.click("现在就办");
  await page.waitForText("清点完了。");
  await page.waitFor("the turn to end", `!document.body.innerText.includes("分身正在运功")`);
  const byHand = mock.requests[7].body;
  const shown = await page.evaluate(`({ title: ${title}, said: document.querySelector(".said-when")?.textContent, chip: [...document.querySelectorAll(".sidebar .row-button[aria-current] .chip")].map((chip) => chip.textContent).join() })`);
  check(shown.title === "晨课" && shown.said.includes("功课交办") && shown.chip === "功课", "started by hand, it opened a 分身 named after it, whose first message is marked as the 功课's");
  check(JSON.stringify(byHand.system).includes("A schedule the user set up started this session") && byHand.messages[0].content[0].text === "清点洞府里的文件", "the model got the task, and was told nobody may be there to answer");

  // One whose time comes starts by itself: nothing is clicked between saving it and its 分身 appearing.
  const two = (value) => String(value).padStart(2, "0");
  const due = new Date(Date.now() + 3000);
  const at = `${due.getFullYear()}-${two(due.getMonth() + 1)}-${two(due.getDate())}T${two(due.getHours())}:${two(due.getMinutes())}:${two(due.getSeconds())}`;
  const timed = { id: "", name: "到点", workspace, prompt: "到点了，报个到", when: { kind: "once", at }, mode: "edit", isolated: false, enabled: true };
  await page.evaluate(`window.__TAURI_INTERNALS__.invoke("save_schedule", { schedule: ${JSON.stringify(timed)} }).then(() => true)`);
  const row = `[...document.querySelectorAll(".sidebar .row-button")].find((row) => row.querySelector(".row-title")?.textContent === "到点")`;
  await page.waitFor("the 功课's own 分身 to appear when its time comes", `!!${row}`);
  check(Date.now() >= due.getTime(), "a 功课 started a 分身 by itself, and not before its time");
  await page.evaluate(`${row}.click()`);
  await page.waitForText("报到。");
  const ran = schedules().find((schedule) => schedule.name === "到点");
  check(!ran.enabled && !ran.next && !!ran.last.session && !ran.last.error && mock.requests[8].body.messages[0].content[0].text === "到点了，报个到", "its 分身 was given the task, and a one-time 功课 is off once it has run");
  await page.click("功课");
  await page.waitForText("办过了，不会再办");
  check((await page.evaluate(`document.querySelectorAll(".schedule .link").length`)) === 2, "the 功课 page shows each one's last start, with the way to its 分身");
  // A fork goes on from another session's conversation, in a session of its own.
  const rowOf = (name) => `[...document.querySelectorAll(".sidebar .row-button")].find((row) => row.querySelector(".row-title")?.textContent === ${JSON.stringify(name)})`;
  const logs = () => readdirSync(join(home, "sessions")).map((id) => readFileSync(join(home, "sessions", id, "session.v1.jsonl"), "utf8").trim().split("\n").map((line) => JSON.parse(line)));
  await page.evaluate(`${rowOf("分头去查")}.click()`);
  await page.waitFor("the session to fork", `${title} === "分头去查" && document.body.innerText.includes("都查完了。")`);
  const before = logs().length;
  await page.click("分叉");
  await page.waitFor("the fork's own page", `document.querySelector(".kin-link")?.textContent.trim() === "分叉" && document.querySelector(".kin-link").title.includes("出自「分头去查」") && document.body.innerText.includes("以上是分叉时带过来的前情")`);
  const forked = logs().find((events) => events[0].data.fork);
  const source = logs().find((events) => events[0].data.id === forked[0].data.fork.session);
  check(logs().length === before + 1 && forked[0].data.fork.turn === 1 && JSON.stringify(forked.slice(1)) === JSON.stringify(source.slice(1)), "forking a session made a new one whose log begins as a copy of the conversation, event for event");
  check((await page.evaluate(`[...document.querySelectorAll(".sidebar .row-button[aria-current] .chip")].map((chip) => chip.textContent).join()`)) === "分叉" && (await page.evaluate(`document.body.innerText.includes("已分叉")`)), "the fork is marked in the list, and says that only the conversation was forked");
  await page.type("#composer-input", "接着查");
  await page.enter();
  await page.waitForText("分叉这边查完了。");
  await page.waitFor("the turn to end", `!document.body.innerText.includes("分身正在运功")`);
  const heard = mock.requests[9].body.messages.filter((message) => message.role === "user").flatMap((message) => message.content).filter((block) => block.type === "text").map((block) => block.text);
  const unchanged = logs().find((events) => events[0].data.id === source[0].data.id);
  check(heard.includes("分头去查") && heard.includes("接着查") && unchanged.length === source.length, "the fork went on from what it copied, and the session it came from stayed as it was");

  // "从这里分叉" on a message forks before it, and brings the message back to the box to send again or change.
  await page.evaluate(`[...document.querySelectorAll(".said-top button")].filter((button) => button.textContent.trim() === "从这里分叉").at(-1).click()`);
  await page.waitFor("a fork from before the second message", `document.querySelector("#composer-input")?.value === "接着查" && document.body.innerText.includes("都查完了。") && !document.body.innerText.includes("分叉这边查完了。")`);
  const refork = logs().find((events) => events[0].data.fork?.session === forked[0].data.id);
  check(refork[0].data.fork.turn === 1 && !refork.some((event) => event.type === "turn/start" && event.data.turn === 2), "a fork from a message holds the turns before it, with the message back in the box");

  // A replay shows the session as it stood, a step at a time, in place of the message box, and changes nothing.
  await page.click("回溯");
  await page.waitFor("the replay bar in place of the message box", `!!document.querySelector(".replay") && document.querySelector("#composer-input").offsetParent === null`);
  await page.evaluate(`(() => { const play = document.querySelector(".replay-play"); if (play.textContent.includes("暂停")) play.click(); })()`);
  await page.waitFor("the replay to stop playing", `document.querySelector(".replay-play").textContent.includes("播放")`);
  await page.evaluate(`(() => { const first = [...document.querySelectorAll(".replay button")].find((button) => button.textContent.trim() === "回到开头"); if (!first.disabled) first.click(); })()`);
  const where = `(document.querySelector(".replay-where")?.textContent ?? "")`;
  await page.waitFor("the first step", `${where}.startsWith("第 1 / ")`);
  const steps = Number((await page.evaluate(where)).match(/第 1 \/ (\d+) 步/)[1]);
  const started = await page.evaluate(`({ asked: document.body.innerText.includes("分头去查"), answered: document.body.innerText.includes("我派一个子分身去查"), ended: document.body.innerText.includes("都查完了。") })`);
  check(steps === 4 && started.asked && !started.answered && !started.ended, "the replay's first step shows the first message and nothing the 分身 did after it");
  for (let step = 1; step < steps; step++) await page.click("下一步");
  await page.waitFor("the last step", `${where}.startsWith("第 ${steps} / ") && document.body.innerText.includes("都查完了。")`);
  await page.click("退出回溯");
  await page.waitFor("the message box to come back with what was in it", `!document.querySelector(".replay") && document.querySelector("#composer-input").offsetParent !== null && document.querySelector("#composer-input").value === "接着查"`);
  check(logs().find((events) => events[0].data.id === refork[0].data.id).length === refork.length, "stepping through the replay reached the end, left the log as it was, and gave the message box back");
  // Commands are confined on Linux and macOS only: here the setting says so, and cannot be switched on.
  await page.click("设置");
  await page.click("禁制");
  await page.waitForText("这个系统上还布不了结界");
  const sandbox = await page.evaluate(`(() => { const box = [...document.querySelectorAll(".settings .check input")].find((input) => input.closest("label").textContent.includes("结界")); return { disabled: box.disabled, checked: box.checked, paths: document.querySelector("#sandbox-paths").closest(".field").hidden }; })()`);
  check(sandbox.disabled && !sandbox.checked && sandbox.paths, "the sandbox setting is off and cannot be switched on where the system has none");
  // A provider is added to the list of keys in a window of its own, and its key is kept.
  await page.click("心法");
  await page.waitFor("the list of providers that are set up", `document.querySelectorAll(".key-list li").length === 1 && document.querySelector(".key-list").textContent.includes("环境变量")`);
  // The button beside the model field asks the provider for its list again.
  const listed = mock.lists.count;
  await page.click("刷新心法列表");
  for (let waited = 0; mock.lists.count === listed && waited < 100; waited++) await new Promise((resolve) => setTimeout(resolve, 100));
  await page.waitForText("问到它提供的 2 门心法");
  check(mock.lists.count === listed + 1, "the refresh button asked the provider for its models again");
  await page.click("新增宗门");
  await page.waitFor("the window that adds one", `document.querySelector("dialog.dialog")?.open`);
  await page.evaluate(`document.querySelector("#key-provider").focus()`);
  await page.waitFor("the 宗门 that can be added", `!!document.querySelector("#key-provider-list")`);
  await page.evaluate(`[...document.querySelectorAll("#key-provider-list li")].find((item) => item.textContent.includes("DeepSeek")).dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true }))`);
  await page.waitFor("the pick", `document.querySelector("#key-provider").value === "DeepSeek" && !document.querySelector("#key-provider-list")`);
  await page.type("#key-value", "e2e-deepseek-key-1234");
  await page.click("保存");
  await page.waitFor("the provider to join the list", `!document.querySelector("dialog.dialog") && document.querySelectorAll(".key-list li").length === 2`);
  check(readFileSync(join(home, "credentials.json"), "utf8").includes("e2e-deepseek-key-1234") && !(await page.evaluate(`document.body.innerText`)).includes("e2e-deepseek-key-1234"), "a key added in the window is saved, and the list shows only its ends");
  // The model field's list holds every model, not only those that match what the field already says.
  await page.evaluate(`document.querySelector("#default-model").focus()`);
  await page.waitFor("the list of models", `document.querySelectorAll(".combo-list li[role=option]").length >= 2`);
  const choices = await page.evaluate(`({ held: document.querySelector("#default-model").value, listed: [...document.querySelectorAll(".combo-list li[role=option] .mono")].map((item) => item.textContent) })`);
  check(choices.listed.includes("claude-e2e-small") && !"claude-e2e-small".includes(choices.held) && choices.listed.includes(choices.held), "the model list offers every model the provider serves, whatever the field holds");
  await page.evaluate(`[...document.querySelectorAll(".combo-list li[role=option]")].find((item) => item.textContent.includes("claude-e2e-small")).dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true }))`);
  await page.waitFor("the pick to be kept", `document.querySelector("#default-model").value === "claude-e2e-small" && !document.querySelector(".combo-list")`);
  // The page shows the pick at once; the file follows when the save comes back.
  const savedModel = () => JSON.parse(readFileSync(join(home, "settings.json"), "utf8")).model;
  for (let waited = 0; savedModel() !== "claude-e2e-small" && waited < 50; waited++) await new Promise((resolve) => setTimeout(resolve, 100));
  check(savedModel() === "claude-e2e-small", "picking a model from the list saves it as the default");
  // The provider said how large that model's context window is when it listed its models; the page says so, and it is kept.
  await page.waitFor("the model's context window as the provider reported it", `document.querySelector("#context-window")?.textContent.includes("400K") && document.querySelector("#context-window").textContent.includes("宗门自己报的")`);
  check(JSON.parse(readFileSync(join(home, "models.json"), "utf8")).anthropic["claude-e2e-small"] === 400000, "a context window the provider reports is shown with its source, and kept for the next run");
  // The model's window is one line; "改" opens the field that corrects it, which comes before what the provider said.
  check(!(await page.evaluate(`!!document.querySelector("#context-window-set")`)), "the field that corrects a model's context window is out of sight until asked for");
  await page.click("改");
  await page.type("#context-window-set", "300000");
  await page.enter();
  await page.waitFor("the window as set by hand", `document.querySelector("#context-window")?.textContent.includes("300K") && document.querySelector("#context-window").textContent.includes("本尊自己填的") && !document.querySelector("#context-window-set")`);
  check(JSON.parse(readFileSync(join(home, "settings.json"), "utf8")).context_windows["anthropic/claude-e2e-small"] === 300000, "a context window set by hand is saved for that model and comes before what the provider reported");
  await page.click("改回自动");
  await page.waitFor("the provider's figure again", `document.querySelector("#context-window")?.textContent.includes("400K") && document.querySelector("#context-window").textContent.includes("宗门自己报的")`);
  check(Object.keys(JSON.parse(readFileSync(join(home, "settings.json"), "utf8")).context_windows).length === 0, "taking it back gives the window to what the provider reported");
  // The limit can follow each model's own window instead of one figure for all.
  const pickLimit = (name) => page.evaluate(`[...document.querySelectorAll("input[name=context-limit-kind]")].find((input) => input.closest("label").textContent.trim() === ${JSON.stringify(name)}).click()`);
  await pickLimit("自动");
  await page.waitFor("the limit to follow the model", `!document.querySelector("#context-limit") && document.body.innerText.includes("用到它的八成，claude-e2e-small 眼下是 320K")`);
  check(JSON.parse(readFileSync(join(home, "settings.json"), "utf8")).context_auto === true, "the limit set to 自动 follows the model's own window: four fifths of it");
  await pickLimit("自己定");
  await page.waitFor("the figure again", `!!document.querySelector("#context-limit")`);
  // The 灵石 menu has the conversation summarized now, without waiting for the limit.
  await page.evaluate(`${rowOf("分头去查")}.click()`);
  await page.waitFor("the session to summarize", `${title} === "分头去查" && document.body.innerText.includes("都查完了。")`);
  await page.click("灵石");
  await page.click("现在就凝成摘要");
  await page.waitForText("凝成了一段摘要");
  const summarizing = mock.requests.at(-1).body;
  // Several sessions are named so; the one on the page is the one whose log now ends in a summary.
  const summarized = logs().filter((events) => events.at(-1).type === "context/compacted");
  check(JSON.stringify(summarizing.messages).includes("Summarize it inside <summary>") && summarized.length === 1 && summarized[0].at(-2).type === "turn/end", "the 灵石 menu had the conversation summarized now, between turns");
  // While the 分身 thinks, its thought is one line that shows the newest words, and stays so.
  await page.type("#composer-input", "再想想");
  await page.enter();
  const live = `(() => { const line = document.querySelector(".thinking-live"); return line && { height: line.getBoundingClientRect().height, full: !!line.querySelector(".tail.full"), end: line.textContent.trim().endsWith("complexity") }; })()`;
  await page.waitFor("the thought to show", `!!${live}?.full`);
  const thoughtNow = await page.evaluate(live);
  await new Promise((resolve) => setTimeout(resolve, 400));
  const thoughtLater = await page.evaluate(live);
  check(thoughtNow.height < 30 && thoughtNow.full && thoughtNow.end && thoughtLater.full, "a long thought is one line, held at its newest words, and stays so");
  await page.waitForText("推演完了。");

  // 全权托付 is offered only where a worktree keeps the work apart, after saying what it means, and shows in cinnabar.
  const modeButton = `[...document.querySelectorAll(".composer-row .select-button")].find((button) => button.querySelector(".label")?.textContent === "禁制")`;
  const bypassOption = `[...document.querySelectorAll(".mode-option.danger")][0]`;
  await page.evaluate(`${modeButton}.click()`);
  await page.waitFor("the 禁制 menu", `!!${bypassOption}`);
  check(await page.evaluate(`${bypassOption}.disabled`), "全权托付 cannot be picked for a 分身 that works in the folder itself");
  await page.evaluate(`${modeButton}.click()`);
  await page.evaluate(`document.querySelector(".group-add.apart").click()`);
  await page.waitFor("a session with a worktree of its own", `document.querySelector(".main-header .branch")?.textContent.trim().startsWith("odg/")`);
  await page.evaluate(`${modeButton}.click()`);
  await page.waitFor("全权托付 to be offered", `${bypassOption} && !${bypassOption}.disabled`);
  await page.evaluate(`${bypassOption}.click()`);
  await page.waitFor("the dialog that says what it means", `document.querySelector("dialog.dialog.danger")?.open`);
  check(!(await page.evaluate(`${modeButton}.classList.contains("danger")`)), "nothing is switched on before the dialog is answered");
  await page.click("确认托付");
  await page.waitFor("the mode to show in cinnabar", `${modeButton}.classList.contains("danger") && document.querySelector(".composer-box").classList.contains("trusted")`);
  await page.type("#composer-input", "写个文件");
  await page.enter();
  await page.waitForText("写好了。");
  const trustedCheckout = readdirSync(join(home, "worktrees")).find((name) => existsSync(join(home, "worktrees", name, "trusted.txt")));
  check(!!trustedCheckout && !(await page.evaluate(`document.body.innerText.includes("请本尊定夺")`)), "left to ask about nothing, a command that writes a file ran without asking, in the worktree");
  check(JSON.parse(readFileSync(join(home, "settings.json"), "utf8")).mode !== "bypass", "全权托付 stays with that 分身: it is not what new ones start in");

  // The 护法: in 放手施为, a model judges what the rules cannot pass. What it will not pass is asked, and declined in time.
  await page.evaluate(`window.__TAURI_INTERNALS__.invoke("get_settings").then((view) => window.__TAURI_INTERNALS__.invoke("save_settings", { settings: { ...view.settings, guardian: true, approval_timeout: 3 } })).then(() => true)`);
  await page.evaluate(`document.querySelector(".group-add:not(.apart)").click()`);
  await page.waitFor("a new session in the folder itself", `${title} === "新分身" && !document.querySelector(".main-header .branch")?.textContent.trim().startsWith("odg/")`);
  await page.evaluate(`${modeButton}.click()`);
  await page.evaluate(`[...document.querySelectorAll(".mode-option")].find((option) => option.textContent.includes("放手施为")).click()`);
  await page.waitFor("放手施为", `${modeButton}.textContent.includes("放手施为")`);
  const guarded = mock.requests.length;
  await page.type("#composer-input", "写两个文件");
  await page.enter();
  await page.waitForText("护法放行：只在洞府里写一个文件。");
  // The verdict shows a moment before the command it let through has run.
  for (let waited = 0; !existsSync(join(workspace, "guarded.txt")) && waited < 100; waited++) await new Promise((resolve) => setTimeout(resolve, 100));
  const review = mock.requests[guarded + 1].body;
  check(existsSync(join(workspace, "guarded.txt")) && JSON.stringify(review.system).includes("You guard a user's computer") && (review.tools ?? []).length === 0 && JSON.stringify(review.messages).includes("echo hi > guarded.txt"), "the 护法 was shown the command and let it run without asking 本尊");
  await page.waitForText("护法没有放行：这一条先问问本尊。");
  check(await page.evaluate(`!!document.querySelector(".sign-countdown")`), "what the 护法 would not pass is put to 本尊, with the time left to answer");
  await page.waitForText("无人定夺，自动驳回");
  await page.waitForText("第二个没写成，等本尊点头。");
  check(!existsSync(join(workspace, "second.txt")), "with nobody answering in time, the request was declined by itself and the 分身 went on without it");

  console.log("Files attached to a message");
  await page.evaluate(`document.querySelector(".group-add:not(.apart)").click()`);
  await page.waitFor("a new session", `${title} === "新分身"`);
  // Dropped on the window, as the system tells the page of it.
  const beforeFiles = mock.requests.length;
  await page.evaluate(`window.__TAURI_INTERNALS__.invoke("plugin:event|emit", { event: "tauri://drag-drop", payload: { paths: ${JSON.stringify([attachedNotes, attachedLog, attachedBinary, attachedPdf])}, position: { x: 600, y: 600 } } }).then(() => true)`);
  await page.waitFor("the files to be attached", `document.querySelectorAll(".attachment.file").length === 3`);
  const pending = await page.evaluate(`({ chips: [...document.querySelectorAll(".attachment.file")].map((chip) => chip.textContent), refused: document.querySelector(".attachments .refused")?.textContent ?? "" })`);
  check(pending.chips[0].includes("要点.md") && pending.chips[1].includes("build.log") && pending.chips[1].includes("分身自读") && !pending.chips[0].includes("分身自读"), "files dropped on the window wait in the box, the large one marked as left for the 分身 to read");
  check(pending.chips[2].includes("sheet.pdf") && pending.chips[2].includes("1 页") && !pending.chips[2].includes("分身自读"), "a PDF is attached, with how many pages it has");
  check(pending.refused.includes("archive.zip") && pending.refused.includes("不是文本文件"), "a file that is not text is not attached, and the box says why");
  await page.type("#composer-input", "看看这两份");
  await page.enter();
  await page.waitForText("两份附件都看过了。");
  await page.waitFor("the turn to end", `!document.body.innerText.includes("分身正在运功")`);
  const [withFiles, afterRead] = mock.requests.slice(beforeFiles).map((request) => request.body);
  const sentBlocks = withFiles.messages[0].content.map((block) => block.text);
  check(
    sentBlocks.length === 4 && sentBlocks[0] === `<attached_file path="${attachedNotes}">\n# 要点\n先读配置\n</attached_file>` && sentBlocks[1].startsWith(`<attached_file path="${attachedLog}" bytes="120023">`) && !sentBlocks[1].includes("first line of the log") && sentBlocks[3] === "看看这两份",
    "the small file's text went to the model with the message, the large one by its path alone, both ahead of the words",
  );
  check(sentBlocks[2].startsWith(`<attached_file path="${attachedPdf}">\n[It is a PDF of 1 page. This is its text`) && sentBlocks[2].includes("Pin 1 is VIN") && sentBlocks[2].includes("Pin 2 is GND"), "the PDF went to the model as the text of its page");
  const readBack = afterRead.messages[afterRead.messages.length - 1].content.find((block) => block.type === "tool_result");
  check(!readBack.is_error && readBack.content.includes("first line of the log") && !(await page.evaluate(`document.body.innerText.includes("请本尊定夺")`)), "the 分身 read the attached file outside the workspace without asking");
  check((await page.evaluate(`[...document.querySelectorAll(".said-files .file-chip-name")].map((name) => name.textContent).join()`)) === "要点.md,build.log,sheet.pdf", "the conversation shows what was attached under the message");

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
