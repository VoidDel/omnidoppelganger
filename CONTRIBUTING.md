# 参与贡献

欢迎反馈问题、提改进建议和提交代码。使用层面的说明请先查阅 [使用手册](doc/user-guide.md) 和 [常见问题](doc/faq.md)。

## 反馈问题

在 [Issues](https://github.com/VoidDel/omnidoppelganger/issues/new/choose) 里选择对应的模板。反馈问题时请尽量写清楚：

- **版本号**（“设置 › 外观 › 版本与更新”）与**操作系统**（Windows 10/11、macOS Apple Silicon、Linux 发行版与包格式）。
- **使用的宗门与心法**（例如 OpenAI / GLM / Gemini / 本机 `claude` 或 `agy` CLI）以及当时的**禁制**（如府内自便、放手施为）。
- **做了什么、期望什么、实际发生了什么**；如果能稳定复现，请列出复现步骤。
- 涉及会话或工具异常时，可以用会话的 **导出 Markdown**（`/export`）附上相关轮次的脱敏记录。**切勿上传含有真实密钥或令牌的 `~/.odg/credentials.json` 与 `~/.odg/mcp.json`**。

发现命令沙箱绕过、路径越界、审批绕过或命令注入等安全漏洞时，请不要公开提 Issue，改用 GitHub 的 [私密安全报告](https://github.com/VoidDel/omnidoppelganger/security/advisories/new)。

## 搭建开发环境

需要 **Rust 1.85+**（使用 Rust 2024 edition）、**Node.js 24**、**pnpm 12**，以及各平台的 [Tauri v2 系统依赖](https://v2.tauri.app/start/prerequisites/)（Windows 需 VS C++ 生成工具与 WebView2；Linux 需 `libwebkit2gtk-4.1-dev` 与 `librsvg2-dev`）。

```bash
pnpm install
pnpm tauri dev      # 启动带热重载的桌面应用
```

只改命令行或核心库时，也可以直接通过 Cargo 运行，无需启动桌面窗口：

```bash
cargo run -p odg-cli -- --help
```

## 代码在哪里

| 路径 | 内容 |
|---|---|
| `crates/odg-core/` | 核心引擎库（**不依赖 Tauri**）：追加式会话日志、LLM 多协议流式客户端与本机 CLI 驱动、工具集、禁制与审批策略、护法、LSP、MCP、秘籍、文房、沙箱、Git 工作树与价格估算 |
| `crates/odg-cli/` | 命令行工具 `odg`，与桌面端共享 `~/.odg/` 状态与核心循环 |
| `src-tauri/` | Tauri 桌面外壳：IPC 命令桥接、事件转发、本地 PTY 终端管理 |
| `src/` | React 19 + TypeScript 前端界面：从会话事件日志纯函数折叠渲染（`src/lib/fold.ts`）、代码工作台、Git 面板、终端与设置 |
| `crates/odg-core/tests/evals.rs` | 确定性 Agent 核心评测集（覆盖多工具编辑、自定义压缩、审批拦截、提示词模板与批处理检索） |
| `e2e/` | Windows WebView2 桌面端到端自动化测试（内置脚本化 Mock LLM 服务） |
| `doc/` | [使用手册](doc/user-guide.md)、[常见问题](doc/faq.md)、[隐私政策](doc/privacy.md)、[工具设计](doc/TOOLS.md)、[Windows 沙箱设计](doc/WINDOWS_SANDBOX.md) 与 [路线图](doc/ROADMAP.md) |

计划做较大改动或引入新工具之前，建议先读一遍 [工具设计说明](doc/TOOLS.md) 和 [路线图](doc/ROADMAP.md)，并先开一个 Issue 对齐设计思路。

## 提交之前

GitHub Actions CI 会运行下面这些检查，提交 PR 前请先在本地跑通：

```bash
pnpm lint                                              # 前端代码风格与 Hooks 静态检查（Biome）
pnpm test                                              # 前端单元测试（Vitest）
pnpm build                                             # TypeScript 类型检查与前端打包（Tauri 编译依赖 dist/）
cargo fmt --all -- --check                             # Rust 代码格式检查
cargo clippy --workspace --all-targets -- -D warnings  # Rust 静态检查（零警告）
cargo test --workspace                                 # 核心库、CLI 与桌面壳全部单元/集成测试
pnpm run eval                                          # 确定性 Agent 端到端场景评测集
node scripts/check-version.mjs                         # 校验全仓清单版本号一致
node scripts/check-docs.mjs                            # 校验内置秘籍、斜杠命令与工具文档同源码一致
```

在 Windows 上修改了桌面交互或前后端主流程时，可以再跑一遍真实窗口端到端测试（跑在隔离临时目录，不碰本机 `~/.odg`）：

```bash
pnpm e2e --build
```

## 设计与编码约定

### 核心架构铁律

1. **追加式会话日志是唯一事实来源**：每个会话只追加写入 `session.v1.jsonl`（历史绝不原地改写）。发给模型的上下文请求（`session.rs`）和前端界面状态（`src/lib/fold.ts`）都必须纯粹从事件序列推导出来；任何模型可见的新输入都必须对应一种明确的会话事件。
2. **每个工具调用必有结果**：无论回合正常结束、被用户中途收回、报错还是异常中断，日志中绝不能留下悬空的工具调用。
3. **新增或修改工具四件套齐全**：在 `crates/odg-core/src/tools/` 增加工具时，必须同步补齐 `policy.rs` 中的五档禁制审批规则、`src/components/ToolCard.tsx` 中的印章与卡片展示，以及对应的自动化测试。
4. **日志兼容性**：对会话日志结构做不向后兼容的调整时，必须递增 `FORMAT_VERSION` 并处理好旧版本读取。

### 界面与前端依赖

- **“符箓”视觉语义**：白纸代表本尊指令，黄符配朱砂印代表分身动作，朱丝栏奏折专用于权限请示；青玉色用于发送/传令，朱砂色仅用于批准与警示，不要混用动作语义色。亮色与暗色两套主题下都要核对可读性，交互控件保证键盘可达（`Esc` 关闭弹层、清晰 `aria-label` 或标题）。
- **前端依赖供应链冷却期**：CI 使用 `pnpm 12`，默认启用新版本发布满 24 小时的安全门禁（`minimumReleaseAge`）。引入或升级 npm 包时避免指向刚发布几小时内的版本，以免锁文件在 CI 安装时被拦截。

### 提交信息与合并请求

提交信息采用 `类型(范围): 说明` 或简明祈使句，类型使用 `feat`、`fix`、`build`、`ci`、`docs`、`refactor`、`test`、`chore`，例如：

```text
feat(core): 支持按境界自动路由快慢心法
fix(pty): 响应 Windows ConPTY 光标位置查询避免测试挂起
```

一个 PR 尽量只聚焦一件事，在描述里写明改了什么、为什么这么改，以及做了哪些验证。

## 发布

Rust 工作区下各 crate（`crates/odg-core`、`crates/odg-cli`、`src-tauri`）统一通过 `version.workspace = true` 继承根目录 `Cargo.toml` 中 `[workspace.package]` 的版本号。维护者发版时运行一键改版脚本同步全仓版本并执行门禁校验：

```bash
node scripts/bump-version.mjs <版本号>   # 一键同步根 Cargo.toml [workspace.package]、package.json、src-tauri/tauri.conf.json 与 packaging/arch/PKGBUILD
cargo check --workspace                  # 更新 Cargo.lock 中的工作区包版本
node scripts/check-version.mjs           # 校验全仓发布清单版本完全一致
node scripts/check-docs.mjs              # 校验文档与内置秘籍/斜杠命令/工具源码一致
```

提交版本变更并推送 `v<版本号>` 标签后，`.github/workflows/release.yml` 会自动校验标签版本，并在 Windows、macOS、Linux（Ubuntu 与 Arch）上构建经签名公证的桌面安装包、三端独立 `odg-cli` 压缩包、`latest.json` 自动更新清单与 `SHA256SUMS.txt` 发布到 GitHub Releases。

## 许可

提交的代码以本仓库的 [MIT 许可](LICENSE) 发布。
