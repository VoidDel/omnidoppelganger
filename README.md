# OmniDoppelganger

OmniDoppelganger（`odg`）是一个跑在你自己电脑上的编码 agent 桌面应用：Rust 核心，Tauri 外壳，React 界面。

你选一个文件夹，交代一件事；分身读文件、搜索、改文件、跑命令。读和改工作区里的文件不用问你，每条命令、每次碰工作区外面的路径，都先等你签字。

## 运行

需要 Rust、Node.js 和 pnpm；Windows 上还需要 Visual Studio 的 C++ 构建工具和 WebView2。

```sh
pnpm install
pnpm tauri dev
```

打包成可执行文件：

```sh
pnpm tauri build
```

启动后在**设置**里填 Anthropic API 密钥，或者启动前导出 `ANTHROPIC_API_KEY`。

## 结构

```
crates/odg-core/   核心库，不依赖桌面外壳
  events.rs        会话事件定义
  session.rs       追加式会话日志，以及从日志推导模型请求
  anthropic.rs     Messages API 流式客户端（SSE 解析、重试）
  tools.rs         四个工具：read_file、search、edit_file、run_command
  policy.rs        哪些操作要先签字
  agent.rs         agent 循环：回合、步骤、审批、插话、停止
  settings.rs      设置和密钥
src-tauri/         Tauri 命令和事件转发
src/               React 界面
```

## 几条设计约定

- **日志是唯一事实来源。** 每个会话是一个只追加的文件 `~/.odg/sessions/<id>/session.v1.jsonl`。发给模型的每个请求都只从日志推导，界面也只从日志折叠出来，所以重开会话看到的和之前完全一样。
- **历史只追加，不改写。** 模型返回的内容块（包括思考块）原样保存、原样回放。
- **每个工具调用都有结果。** 无论一轮是正常结束、被停止、出错还是应用崩溃，日志里不会留下没有结果的工具调用。
- **界面颜色只表达一件事。** 白纸是你说的，黄纸是分身做的，粉纸等你签字。

## 环境变量

| 变量 | 作用 |
|---|---|
| `ANTHROPIC_API_KEY` | 没有在设置里保存密钥时使用 |
| `ANTHROPIC_BASE_URL` | 把请求发到自己的网关；此时不发送只有官方接口支持的字段 |
| `ODG_HOME` | 会话、设置和密钥的存放目录，默认 `~/.odg` |

## 测试

```sh
cargo test -p odg-core
```

核心库的测试不需要 API 密钥：模型由脚本化的替身扮演，工具在临时目录里真实执行。

## 许可证

[MIT](LICENSE)
