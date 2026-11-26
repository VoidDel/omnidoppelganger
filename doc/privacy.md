# Privacy Policy · 隐私政策

*English first; [中文在下](#隐私政策)。*

OmniDoppelganger (`odg`) is a local-first desktop and CLI coding agent. We do not operate any backend servers for the app, require no account registration, and collect no telemetry, analytics, or crash reports.

## What stays on your computer

Everything the app stores lives on your own machine under `~/.odg/` (Windows `%USERPROFILE%\.odg`, or the directory named by `ODG_HOME`):

- **Conversations, tool history, and undo snapshots**: append-only JSONL logs under `sessions/<id>/session.v1.jsonl` together with per-turn file undo snapshots.
- **Settings, grants, schedules, skills, and prompt templates**: `settings.json`, `grants.json`, `schedules.json`, `skills/`, and `prompts/`.
- **Local usage counters, model cache, and MCP memory**: `model-usage.json` (local per-model token totals and rate-limit/quota cooldown timestamps), `models.json` (cached model context-window sizes reported by providers), and `mcp-memory.jsonl` (created only when you enable the built-in local `memory` MCP server).
- **Worktrees, standalone chat, pastes, temporary staging, tools, and notification icon**: `worktrees/` (isolated Git worktrees), `闲谈/` (standalone chat workspace), `pastes/` (collapsed long-paste attachments, automatically pruned after 7 days or when removed from a draft), `tmp/` (private runtime staging for CLI IPC, skill installs, and snapshots instead of shared `/tmp`), `lsp/` and `runtime/` (optional downloaded language servers and Python document runtime), and `app-icon.png` (materialized icon on Windows/Linux for native desktop notifications).
- **API keys and Unix `0700` / `0600` private permissions**: API keys are stored in your operating system's credential manager (`Windows Credential Manager`, `macOS Keychain` bound to the Developer ID Team ID ACL on signed builds, or `Linux Secret Service`) under `OmniDoppelganger`. Only when the OS credential store is unavailable—or when `ODG_HOME` is explicitly set—are keys written to `credentials.json`. Tokens written inside `mcp.json` follow the standard plain-text MCP configuration format. On macOS and Linux (Unix), `~/.odg/` and sensitive subdirectories (`sessions/`, `pastes/`, `tmp/`) are enforced with `0700` owner-only permissions, and sensitive files (`credentials.json`, `mcp.json`) are atomically written with `0600` permissions while refusing dangling symlinks.

## What leaves your computer (and local CLI probing)

External network requests are made **only** to services you configure or explicitly trigger/approve:

1. **Your chosen model provider or local CLI**: When you send a message, the conversation context, attached files/images, and tool outputs are sent directly to the LLM API endpoint you configured (e.g., Anthropic, OpenAI, Google Gemini, OpenRouter, DeepSeek, Moonshot, Zhipu GLM, or your custom/local endpoint) or handed to the local coding CLI you selected (`agy`, `codex`, `claude`, `cursor-agent`, `grok`, `opencode`), governed by that service's own privacy policy.
2. **Model catalog discovery and local CLI / gateway quota checks**:
   - **Provider model listing (`/v1/models`, etc.)**: When you open **Settings** or the **Model Switcher** (`/model`), the app requests the available model list **only** from providers for which you have already configured an API key (plus keyless local endpoints such as Ollama or Magpie), and caches reported context-window limits in `models.json`. Unconfigured cloud providers are never contacted.
   - **Local `agy` CLI login & quota status**: When **Google Antigravity (`agy`)** is viewed in Settings or selected in a session / the usage popover (`/usage`), the app invokes your local `agy` binary (`agy -p /usage --output-format json`) to inspect local sign-in status and remaining 5-hour / weekly subscription quota windows (and similarly queries `/v1/magpie/quotas` if you select a Magpie gateway).
3. **Web search and URL fetching (`web_search`, `fetch_url`)**: Sent only after you approve the request (or grant a persistent rule). `web_search` queries the configured search provider (Zhipu Search API, or Anthropic/OpenAI built-in web search); `fetch_url` requests the exact URL you approved.
4. **Remote MCP servers and image generation**: When you enable a remote MCP server or approve an MCP tool, `read_mcp_resource`, or `generate_image` call, parameters for that call are sent to the configured server or image API.
5. **On-demand tool downloads and update checks (no silent background upgrades)**:
   - **App update checks**: Clicking **Check for updates** in Settings (or confirming an Ed25519-verified in-app update) queries GitHub's public Releases (`https://api.github.com/repos/VoidDel/omnidoppelganger/releases/latest`). No silent background update polling is performed without your action.
   - **Language Servers (`~/.odg/lsp/`) and Python Document Runtime (`~/.odg/runtime/`)**: Optional language servers and the Python document runtime are **never silently downloaded or auto-upgraded in the background by default**. Automatic runtime library updates are controlled by the `auto_update_runtime` setting (off by default); network downloads from GitHub Releases and PyPI (or your configured `ODG_PYTHON_MIRROR` / `PIP_INDEX_URL` mirror) occur **only** when you click install/update, approve `document_runtime`, or explicitly enable `auto_update_runtime` in Settings, and every archive or wheel is verified against pinned cryptographic SHA-256 checksums locally.

## Removing your data

Uninstalling the application binary does not delete `~/.odg/`. To remove all local data, delete `~/.odg/` and remove any `OmniDoppelganger` entries from your OS credential manager.

## Contact

Questions about privacy can be raised in [GitHub Issues](https://github.com/VoidDel/omnidoppelganger/issues).

---

# 隐私政策

OmniDoppelganger（`odg`）是一款本地优先的编码 Agent 桌面与命令行工具。开发者不运营任何应用后端服务器，不需要注册账号，**没有任何遥测统计、行为分析、广告追踪或自动崩溃上报**。

## 留在你电脑上的数据

应用产生的全部数据都保存在你本机的 `~/.odg/` 目录下（Windows 为 `%USERPROFILE%\.odg`，若设置了 `ODG_HOME` 则在该目录）：

- **会话记录与撤销快照**：每段对话、工具调用和结果及每轮文件撤销快照以只追加方式保存在 `sessions/<id>/session.v1.jsonl` 及对应会话目录中，不同步到任何云端。
- **设置、通行符、功课、秘籍与提示词模板**：保存在 `settings.json`、`grants.json`、`schedules.json`、`skills/` 和 `prompts/`。
- **本地用量统计、模型元数据缓存与跨会话记忆**：`model-usage.json`（本地记录各模型累计 Token 用量与限流/额度冷却截止时间，不上传任何服务器）、`models.json`（供应商返回的模型上下文窗口大小缓存）以及 `mcp-memory.jsonl`（仅在你主动启用内置 `memory` 本机 MCP 法器时创建）。
- **工作树、闲谈、长文本粘贴、私有临时目录、扩展运行时与通知图标**：`worktrees/`（独立工作树分身检出目录）、`闲谈/`（闲谈会话工作目录）、`pastes/`（输入框粘贴超长文本生成的本地附件，超 7 天或移出草稿时自动清理）、`tmp/`（用于本机 CLI 通信、秘籍安装与快照中转的私有临时目录，不写入系统公共 `/tmp`）、`lsp/` 与 `runtime/`（按需下载的独立语言服务与文房 Python 环境），以及 `app-icon.png`（Windows / Linux 原生桌面通知使用的本地应用图标缓存）。
- **API 密钥与 Unix `0700` / `0600` 私有权限保护**：密钥默认加密存入操作系统的凭据管理器（Windows 凭据管理器、macOS 钥匙串——正式签名包自动绑定 Developer ID Team ID 访问控制、Linux Secret Service），归在 `OmniDoppelganger` 名下；仅当系统凭据管理器不可用或显式指定了 `ODG_HOME` 时，才会写入本地 `credentials.json`。按通用 MCP 规范写在 `mcp.json` 里的第三方服务令牌与请求头为明文。在 macOS 与 Linux（Unix）系统下，`~/.odg/` 根目录及 `sessions/`、`pastes/`、`tmp/` 等子目录统一强制设为 `0700`（仅当前用户本人可访问），`credentials.json` 与 `mcp.json` 等敏感配置文件统一以 `0600` 权限原子写入并拒绝悬空符号链接。

## 会发往本机之外的请求与本机探测时机

只有你主动配置、触发或明确点头批准的操作才会发起网络请求或本机 CLI 探测：

1. **你选定的模型供应商或本机 CLI**：传令对话时，当前会话的消息、你附上的文件/图片以及工具返回结果会直接发往你所选宗门的接口地址（如 Anthropic、OpenAI、Google Gemini、OpenRouter、DeepSeek、Kimi、智谱 GLM 或你自填的网关），或交给你选定的本机编码 CLI（`agy`、`codex`、`claude`、`cursor-agent`、`grok`、`opencode`），受对应服务自身的隐私条款约束。
2. **模型列表拉取与本机 CLI / 网关额度探测**：
   - **拉取可用模型列表（`/v1/models` 等）**：当你打开 **设置页** 或点开 **模型切换器**（含 `/model`）时，应用仅向**已配置 API 密钥**的云端供应商（以及 Ollama、Magpie 等免密本地服务）请求可用模型清单（如 `GET /v1/models`），并将返回的模型上下文窗口大小缓存在本地 `models.json`；未配置密钥的云端供应商绝不会收到请求。
   - **查询本机 `agy` CLI 登录与订阅额度状态**：当你在设置页查看或在会话/用量面板（`/usage`）选用 **Google Antigravity（`agy`）** 时，应用会在本机调用 `agy` 可执行文件（`agy -p /usage --output-format json`）读取登录状态及 5 小时 / 周度剩余额度（选用 Magpie 网关时同理向所配网关请求 `/v1/magpie/quotas`）。
3. **联网搜索与网页读取（`web_search`、`fetch_url`）**：每次动用前均需你批准（或你已授予长期通行符）。`web_search` 向所选搜索服务发送查询词；`fetch_url` 直接请求你批准的目标网址。
4. **远程 MCP 法器与丹青生图**：连接你启用的远程 MCP 服务，或在批准调用 MCP 工具、`read_mcp_resource`、`generate_image` 时向对应服务端发送调用参数。
5. **按需组件下载与检查更新（默认不静默联网升级）**：
   - **检查应用更新**：在“设置 › 外观 › 版本与更新”点击 **检查更新**（或确认执行经 Ed25519 签名校验的应用内更新）时，向 GitHub 公开接口（`api.github.com` / `github.com`）查询或下载最新发布版本；不点不请求。
   - **语言服务（`~/.odg/lsp/`）与文房 Python 运行时（`~/.odg/runtime/`）**：**默认绝不在后台静默联网自动升级**。文房依赖库随版本变更的自动补装受设置项 `auto_update_runtime` 控制（默认关闭）；仅在你主动点击安装/更新、在对话中批准 `document_runtime` 工具，或在设置中显式开启 `auto_update_runtime` 时，应用才会向官方发布源（GitHub Releases、PyPI 或你通过 `ODG_PYTHON_MIRROR` / `PIP_INDEX_URL` 配置的镜像源）下载对应文件并在本机严格核验 SHA-256 校验码。

## 彻底删除数据

卸载桌面应用不会自动删除 `~/.odg/`。如需彻底清除本机留存的一切记录，手动删除 `~/.odg/` 目录，并在系统凭据管理器中删除 `OmniDoppelganger` 相关条目即可。

## 联系方式

如有隐私相关疑问，欢迎在 [GitHub Issues](https://github.com/VoidDel/omnidoppelganger/issues) 提出。
