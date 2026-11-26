# Privacy Policy · 隐私政策

*English first; [中文在下](#隐私政策)。*

OmniDoppelganger (`odg`) is a local-first desktop and CLI coding agent. We do not operate any backend servers for the app, require no account registration, and collect no telemetry, analytics, or crash reports.

## What stays on your computer

Everything the app stores lives on your own machine under `~/.odg/` (or the directory named by `ODG_HOME`):

- **Conversations and tool history**: append-only JSONL logs under `~/.odg/sessions/<id>/session.v1.jsonl` and undo file snapshots.
- **Settings, grants, schedules, skills, and prompt templates**: `settings.json`, `grants.json`, `schedules.json`, `skills/`, and `prompts/`.
- **API keys**: stored in your operating system's credential manager (`Windows Credential Manager`, `macOS Keychain`, or `Linux Secret Service`) under `OmniDoppelganger`. Only when the OS credential store is unavailable—or when `ODG_HOME` is explicitly set—are keys written to `~/.odg/credentials.json`. Note that tokens written inside `~/.odg/mcp.json` follow the standard plain-text MCP configuration format.

## What leaves your computer

Network requests are made **only** to services you configure or explicitly approve:

1. **Your chosen model provider or local CLI**: When you send a message, the conversation context, attached files/images, and tool outputs are sent directly to the LLM API endpoint you configured (e.g., Anthropic, OpenAI, Google Gemini, OpenRouter, DeepSeek, Moonshot, Zhipu GLM, or your custom/local endpoint) or handed to the local coding CLI you selected (`agy`, `codex`, `claude`, `cursor-agent`, `grok`, `opencode`), governed by that service's own privacy policy.
2. **Web search and URL fetching (`web_search`, `fetch_url`)**: Sent only after you approve the request. `web_search` queries the configured search provider (Zhipu Search API, or Anthropic/OpenAI built-in web search); `fetch_url` requests the exact URL you approved.
3. **Remote MCP servers and image generation**: When you enable a remote MCP server or approve an MCP tool / `generate_image` call, parameters for that call are sent to the configured server or image API.
4. **On-demand tool downloads and update checks**:
   - Clicking **Check for updates** in Settings queries GitHub's public Releases API (`https://api.github.com/repos/VoidDel/omnidoppelganger/releases/latest`). No automatic background update polling is performed without your action.
   - Installing optional Language Servers (`~/.odg/lsp/`) or the Python Document Runtime (`~/.odg/runtime/`) downloads official archives from GitHub Releases and PyPI (or your configured mirror) and verifies their cryptographic checksums locally.

## Removing your data

Uninstalling the application binary does not delete `~/.odg/`. To remove all local data, delete `~/.odg/` and remove any `OmniDoppelganger` entries from your OS credential manager.

## Contact

Questions about privacy can be raised in [GitHub Issues](https://github.com/VoidDel/omnidoppelganger/issues).

---

# 隐私政策

OmniDoppelganger（`odg`）是一款本地优先的编码 Agent 桌面与命令行工具。开发者不运营任何应用后端服务器，不需要注册账号，**没有任何遥测统计、行为分析、广告追踪或自动崩溃上报**。

## 留在你电脑上的数据

应用产生的全部数据都保存在你本机的 `~/.odg/` 目录下（Windows 为 `%USERPROFILE%\.odg`，若设置了 `ODG_HOME` 则在该目录）：

- **会话记录与撤销快照**：每段对话、工具调用和结果以只追加日志保存在 `~/.odg/sessions/<id>/session.v1.jsonl`，不同步到任何云端。
- **设置、通行符、功课、秘籍与提示词模板**：保存在 `settings.json`、`grants.json`、`schedules.json`、`skills/` 和 `prompts/`。
- **API 密钥**：默认加密存入操作系统的凭据管理器（Windows 凭据管理器、macOS 钥匙串、Linux Secret Service），归在 `OmniDoppelganger` 名下；仅当系统凭据管理器不可用或显式指定了 `ODG_HOME` 时，才会写入本地 `credentials.json`。注意：按通用 MCP 规范写在 `~/.odg/mcp.json` 里的第三方服务令牌为明文。

## 会发往本机之外的请求

只有你主动配置或明确点头批准的操作才会发起外部网络请求：

1. **你选定的模型供应商或本机 CLI**：传令对话时，当前会话的消息、你附上的文件/图片以及工具返回结果会直接发往你所选宗门的接口地址（如 Anthropic、OpenAI、Google Gemini、OpenRouter、DeepSeek、Kimi、智谱 GLM 或你自填的网关），或交给你选定的本机编码 CLI（`agy`、`codex`、`claude`、`cursor-agent`、`grok`、`opencode`），受对应服务自身的隐私条款约束。
2. **联网搜索与网页读取（`web_search`、`fetch_url`）**：每次动用前均需你批准（或你已授予长期许可）。`web_search` 向所选搜索服务发送查询词；`fetch_url` 直接请求你批准的目标网址。
3. **远程 MCP 法器与丹青生图**：连接你启用的远程 MCP 服务，或在批准调用 MCP 工具、`generate_image` 时向对应服务端发送调用参数。
4. **按需组件下载与手动检查更新**：
   - 在“设置 › 外观 › 版本与更新”点击 **检查更新** 时，向 GitHub 公开接口（`api.github.com`）查询最新发布版本；不点不请求。
   - 在“设置 › 神通”或经你批准安装语言服务（`~/.odg/lsp/`）与文房运行时（`~/.odg/runtime/`）时，仅向官方发布源（GitHub、PyPI 或你自配的镜像源）下载安装包并在本机核验哈希校验码。

## 彻底删除数据

卸载桌面应用不会自动删除 `~/.odg/`。如需彻底清除本机留存的一切记录，手动删除 `~/.odg/` 目录，并在系统凭据管理器中删除 `OmniDoppelganger` 相关条目即可。

## 联系方式

如有隐私相关疑问，欢迎在 [GitHub Issues](https://github.com/VoidDel/omnidoppelganger/issues) 提出。
