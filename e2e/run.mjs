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
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
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
const hello = join(workspace, "hello.txt");

// What the stand-in model says: create a file and run a command, then wrap up.
const mock = await startMock([
  [
    { text: "我先建文件，再跑命令。" },
    { tool: "edit_file", input: { path: "hello.txt", old_string: "", new_string: "你好，分身\n" } },
    { tool: "run_command", input: { command: "echo e2e-ok" } },
  ],
  [{ text: "都办好了。" }],
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
  await page.click("选文件夹作洞府，化出分身");
  await page.type("#composer-input", "建一个 hello.txt，再跑个命令");
  await page.enter();
  step("a session opens in the workspace and takes a message");

  // The edit runs on its own in the default mode; the command waits for the user.
  await page.waitForText("准这一次");
  check(readFileSync(hello, "utf8") === "你好，分身\n", "the edit wrote hello.txt while the command still waited for approval");
  await page.click("准这一次");
  await page.waitForText("都办好了。");
  await page.waitFor("the turn to end", `!document.body.innerText.includes("分身正在运功")`);
  step("the approved command ran and the model's closing words are shown");

  const [first, second] = mock.requests.map((request) => request.body);
  check(mock.requests.length === 2 && mock.requests[0].headers["x-api-key"] === "e2e-key", "the model was asked twice, with the key");
  check(first.messages[0].content.some((block) => block.text === "建一个 hello.txt，再跑个命令"), "the first request carries the user's message");
  const results = second.messages[second.messages.length - 1].content.filter((block) => block.type === "tool_result");
  check(results.length === 2 && results.every((result) => !result.is_error), "the second request carries both tool results");
  check(results[1].content.includes("e2e-ok") && !results[1].content.includes("\u001b"), "the command's output reached the model without color codes");

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
