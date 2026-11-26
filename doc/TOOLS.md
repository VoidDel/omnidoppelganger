# 内置工具对照

分身的内置工具与 Claude Code、Codex 的能力对照及演进记录。基线更新至 `v1.0.0`（初稿写于 v0.4.3）。

"本项目"一列对照代码核实过（`crates/odg-core/src/tools.rs`、`crates/odg-core/src/tools/`、`agent.rs`、`mcp.rs`）。Claude Code 和 Codex 两列凭对它们的了解写成，没有逐一对照最新版本核实，用之前先查一下。

工作量的估法和[路线图](ROADMAP.md)一样：小是几天，中是一两周，大是数周。

## 对照

| 能力 | 本项目 | Claude Code | Codex |
|---|---|---|---|
| 读、搜、找、改、写文件，跑命令 | 有（`read_file`、`search`、`find_files`、`edit_file`、`write_file`、`run_command`） | 有 | 有（编辑用 `apply_patch`） |
| 批量只读探查 | 有：`batch_inspect`，单次调用合并最多 16 条读/搜/列查询并按字符预算返回紧凑摘要 | 无单独合并工具（靠并行工具调用） | 无单独合并工具 |
| 后台命令：读输出、停掉 | 有（`command_output`、`stop_command`） | 有 | 有 |
| 看图片文件 | 有：`read_file` 把图片交给能看图的模型（v0.4.4 起） | Read 能读图片 | `view_image` |
| 删除、移动、重命名文件 | 有：`delete_file`、`move_file`，可撤回（v0.4.4 起） | 走 Bash | `apply_patch` 能增删和移动 |
| 联网搜索 | 有：GLM 密钥走智谱；没有时 Anthropic、OpenAI 用自家的搜索（v0.4.5 起） | 内置 | 内置 |
| 取网页 | 有（`fetch_url`） | 有 | 无单独工具 |
| 计划清单 | 有（`update_todos`） | 有 | 有 |
| 子分身 | 有：`delegate` 一次最多并行派 8 个子分身，支持 `read_only: true` 只读探查型子分身；暂不支持脱离主回合后台运行与单独续聊 | 有，分类型，可在后台跑、可续聊 | 新版有 |
| 问用户问题 | 有（`ask_user`，v0.4.3 起） | 有 | 有 |
| 呈上计划后切换禁制 | 有：`present_plan`，计划单独成卡，这一轮随之结束（v0.4.8 起） | 有专门工具 | 无 |
| 代码导航（跳到定义、查引用、看诊断） | 有：`code_nav`，接本机或按需下载的语言服务（v0.4.6 起） | 有 LSP 工具 | 无 |
| 给运行中的命令输入 | 有：`command_input`，写进后台命令的标准输入（v0.4.8 起） | 无 | 有 `write_stdin` |
| MCP 资源 | 有：`list_mcp_resources`、`read_mcp_resource`（v0.4.8 起） | 有 | 有 |
| 编辑 Jupyter 笔记本 | 无 | 有 | 无 |
| 模型自己安排定时任务和唤醒 | 无：功课由用户在界面里创建 | 有 | 无 |

本项目也有几样另外两家默认没有的：单次字符预算受控的批量只读探查（`batch_inspect`）、生成和读取 Office 与 PDF 文档的独立运行时（`document_runtime`）、把产物显式交付给本尊（`deliver_files`）、丹青生图（`generate_image`）、按需安装秘籍与法器（`install_skill`、`add_mcp_server`）、护法模型审命令。

## 第一优先

| 事项 | 工作量 |
|---|---|
| ~~看图：`read_file` 读 PNG、JPEG、GIF、WebP，把图片交给能看图的模型~~ 已完成。按文件头认图，不看扩展名；已知不能看图的模型会被告知图是什么、该怎么办；超过约 3.7 MB 的图要先缩小。Anthropic 把图放在工具结果里，OpenAI 的两种接口放在工具结果之后的用户消息里。对话里在这一步下面显示这张图 | 中 |
| ~~删除和移动文件：加 `delete_file`、`move_file`，走和 `edit_file` 一样的批准规则、撤回和改动记录~~ 已完成。一次一个文件，不收文件夹和链接；删除或移动前留一份副本，超过 50 MB 的不收。移动不覆盖已有文件，跨磁盘时改为复制后删除。撤回能把删掉的、挪走的放回原处，但移动时新建的空文件夹会留下 | 中 |
| ~~搜索不依赖 GLM 密钥：没存 GLM 密钥时，改用当前服务商自带的搜索（Anthropic、OpenAI 都有）~~ 已完成。`web_search` 仍是应用自己的工具，每次照旧先请示；没有 GLM 密钥时另发一次请求给分身所用的那一家，让它用自家的搜索工具（Anthropic 的 `web_search_20260209`，较旧的模型用 `web_search_20250305`；OpenAI 的 `web_search`）搜完，再把找到的网页和引文整理成和智谱一样的结果列表。DeepSeek、Kimi 和自定义服务没有可用的自带搜索，仍要 GLM 密钥 | 中 |

## 第二优先

| 事项 | 工作量 |
|---|---|
| ~~批量只读探查（`batch_inspect`）：将多步摸底合并为单次受字符预算约束的批处理调用，减少多轮往返与上下文膨胀~~ 已完成。单次 `queries` 数组按序执行 1～16 条只读子查询（支持 `read_file`、`search`、`find_files` 三种 `kind`），单条结果默认保留 4,000 字符（`max_chars` 上限 20,000），整批汇总默认上限 32,000 字符（`max_total_chars` 上限 80,000），超限时自动截断并标注剩余未执行项；界面工具卡片印章为「览」。审批策略逐项检查 `queries` 中所有路径，凡涉及洞府外路径均按 `outside_read` 先行请示 | 小 |
| ~~代码导航：跳到定义、查引用、看编译器诊断。要按语言接 rust-analyzer、tsserver 这类语言服务~~ 已完成。`code_nav` 一个工具七种问法：定义、引用、实现、类型和文档、文件大纲、按名找符号、一个文件的错误和警告。支持 Rust（rust-analyzer）、C/C++（clangd）、TypeScript/JavaScript（typescript-language-server / TypeScript 7 原生 LSP）、Python（pyright / ty）、Go（gopls），按洞府和语言共享常驻实例并在应用关闭时统一回收。除 `PATH` 上的 typescript-language-server 会加载项目内的 TypeScript（启动前需请示，可"此府永准"）外，其余均配置为不执行项目代码 | 大 |
| ~~语言服务按需下载~~ 已完成（Rust、TypeScript/JavaScript、Python）：没装时由应用经本尊同意下载，按内置版本和校验码核对后放进 `~/.odg/lsp`（分身通过 `language_server` 工具查看或下载，设置的「神通」里也能一键下载和移除）：Rust 下载 `rust-analyzer`（约 15 MB）；TypeScript/JavaScript 下载 TypeScript 7 原生包（约 9–10 MB，`tsc --lsp --stdio`，`runExternalCode` 关闭且 `npm_config_offline=true`，无需请示）；Python 下载 Astral `ty`（约 14–18 MB，`ty server`）。三者均为独立原生程序，不依赖本机 Node 或 Python。C/C++（`clangd`）与 Go（`gopls`）仍由用户自行安装 | 中 |
| ~~给后台命令写输入（`command_input`）：交互式脚手架、REPL、要按 y 确认的命令现在只能绕开~~ 已完成。后台命令的标准输入留着不关，`command_input` 往里写一行（默认带回车），也可以结束输入，写完等一会儿把它的回应一并带回来；前台命令仍然没有输入，问话会立刻失败而不是卡住。每次输入都先请示，请示上写着是哪条命令、打进去什么，"之后都准"只管这一个后台命令，不能永准。写进去的是管道不是伪终端（完整交互式终端见右侧「终端」面板） | 中 |
| ~~呈上计划（`present_plan`）：先谋后动用专门工具呈交方案卡片~~ 已完成。先谋后动下分身用它呈上计划（Markdown，可带标题），对话里单独成一张计划卡片，这一轮随之结束，不再多问模型一次；"照计划动手"的按钮以它为准。别的禁制下调用它会被告知直接动手。子分身不提供 | 小 |
| ~~MCP 资源（`list_mcp_resources`、`read_mcp_resource`）~~ 已完成。有法器提供资源时才提供这两个工具。列出（含地址模板、分页）不用请示；读要先请示，因为地址会发给那个服务，可以对这个法器"之后都准"或"此府永准"。读到的文字照给，图片交给能看图的模型，别的二进制内容只说明是什么 | 小 |
| ~~只读探查型子分身（`delegate` `read_only: true`）~~ 已完成。主分身调用 `delegate` 拆派任务时可为单个子任务指定 `"read_only": true`：该子分身的工具表自动剥离 `edit_file`、`write_file`、`delete_file`、`move_file`、`generate_image`、`install_skill`、`add_mcp_server` 等一切写操作工具，且执行 `run_command` 时强制经 `safety::check_read_only` 校验，即便父分身处于「全权托付」（`Mode::Bypass`）下也严禁修改任何文件，专用于并发代码走读、架构摸底与安全审计 | 小 |

## 第三优先

看需要再做。

| 事项 | 工作量 |
|---|---|
| 编辑 Jupyter 笔记本 | 小 |
| 分身自己建功课、定时回来查看结果 | 中 |
| 子分身脱离主回合在后台异步运行、完成后支持单独续聊（只读探查型 `read_only: true` 已完成） | 中 |
| MCP 工具很多时按需加载工具定义，省上下文 | 中 |
