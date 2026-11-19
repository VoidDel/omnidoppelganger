# Windows 结界（命令沙箱）方案

读了两家的实现后定的方案：

- [openai/codex](https://github.com/openai/codex) 的 `codex-rs/windows-sandbox-rs`（Apache-2.0，读的是 2026-11-19 的 `6271f35f52`）。
- [deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) 的 `packages/sandbox/sandbox-windows-acl`（MIT，读的是 2026-11-18 的 `2e39e70496`）。

"两家怎么做"一节是它们的源码和文档里写的，**都没有在本机验证过**；标"推断"的是代码没明说、从结构推出来的。之后是我们的设计。

## 结论

第一版照 Codex 的**免管理员**做法：从自己的令牌削出一个"写受限"令牌，给洞府加一条权限项。不建账号、不要管理员权限、不装驱动、不打完整性标签。

它能做到的和 Linux、macOS 上的结界一样：命令连同它启动的一切，只能往洞府、临时目录和另填的目录里写。

它做不到的要如实写给用户：

1. **管不住读文件和联网。** 两家都一样，不换账号就管不了。
2. **所有人都可写的目录，结界里照样能写**（推断，见下）。
3. **Node 用管道起子进程可能失败。** 这一条决定它实不实用，要先验证。

管得住读和联网的做法（另建本地账号加防火墙规则）要管理员权限装一次，还要签名和安装包，放到以后。

> **2026-11-19 验证结果：这一版没过第一问。** 写受限令牌下 Node 用管道起子进程全部失败，vitest 跑不了；deepseek 的"写受限加低完整性"也一样。能跑通的只有第三种"纯低完整性"（不加限制名单）。详见下面"第一步的验证结果"。**Windows 结界暂时搁置**，以后再从纯低完整性和管理员安装版里选；下面的设计留作参考，没有按验证结果改。

## 第一步的验证结果

2026-11-19 在这台机器上做（Windows 11 26300，普通用户、中完整性，不提权）。原型是一个约三百行的启动器，放在仓库外，没有进主分支。比了三种令牌：

- **写受限**：Codex 的免管理员做法，即上面"第一版的做法"。
- **写受限加低**：deepseek 的做法。
- **纯低**：令牌降到低完整性、不加限制名单，洞府打"低"标签。

### 五个问题

**1. 常用工具能不能跑通？**

| | 写受限 | 写受限加低 | 纯低 |
|---|---|---|---|
| `git commit` | 通过 | 没测 | 通过 |
| `cargo build`、`cargo test`（odg-core） | 通过 | 没测 | 通过 |
| `pnpm build`（tsc、vite） | 通过 | 没测 | 通过 |
| `pnpm test`（vitest） | **失败**，工作进程超时 | 没测 | 通过，26 条 |
| Node 用管道起子进程 | **失败**，`EPERM` | **失败**，`EPERM` | 通过 |
| `pnpm install` | 失败 | 没测 | 失败 |

Node 失败的根源：结界里能建命名管道，却连不上自己建的管道。libuv 建管道不带安全描述符，系统给的默认权限只让创建者账号、管理员和 SYSTEM 写，Everyone 只能读；写受限令牌的写操作还要过一遍限制名单，名单里只有能力 SID、登录 SID 和 Everyone，于是被拒。令牌的默认权限对命名管道不起作用，改了也一样。Codex 的冒烟测试把"不能建命名管道"当作要的结果；它的管理员版把沙箱账号本身放进了名单，所以那边 Node 能跑。

`pnpm install` 三种都失败，原因与令牌无关：pnpm 要在 `%LOCALAPPDATA%\pnpm-store-operation-locks` 建锁文件，那里在洞府外。这和 Linux、macOS 上一样，是"pnpm 的仓库算不算可写目录"的问题。

**2. 从没有控制台的界面进程起会怎样？** 没问题。仿照应用的起法（`CREATE_NO_WINDOW`、管道输出）套一层启动器，启动器再起 PowerShell，子进程带或不带 `CREATE_NO_WINDOW` 都正常；纯低也正常，deepseek 记录的 `0xC0000142` 没有出现。

另外两个和 PowerShell 有关的发现：

- **应用商店装的 pwsh 起不了**。执行别名和包里的真实路径都被拒，三种令牌都一样；Codex 源码里也有注释说商店应用起不了。这台机器的 pwsh 正是商店装的，结界里只能退回系统自带的 Windows PowerShell 5.1，或者要求装非商店版的 pwsh。
- **`TEMP` 必须指到可写的地方**。否则 PowerShell 启动时写不进探测文件，会降到受限语言模式，连 `[IO.Directory]::Delete` 这类调用都不让用。指到私有临时目录就恢复正常。

**3. 重定向好不好用？** `> $null`、`cmd /c "echo x > NUL"` 都正常，不用像 Codex 那样给空设备单独授权。`Out-File NUL` 失败，但不在结界里也失败，是 5.1 自己的毛病。

**4. Everyone 可写的目录挡不挡得住？** 挡不住，从"推断"变成了实测。在 84 个常见目录（`C:\`、`D:\`、`C:\ProgramData`、`C:\Users\Public` 下两层以内）里试建文件，写受限能写进 3 个第三方软件留下的目录：`C:\ProgramData\boost_interprocess`、`JKI`、`ZulerCoreTools`。纯低一个都写不进。更深的目录没有查。

**5. 第一次登记要多久，撤销能否复原？**

- **快。** 7177 个文件 0.44 秒，第二次也差不多（原型没有做"已有就跳过"）。这个仓库连 `target` 和 `node_modules` 共 31361 个文件，照比例约 2 秒。
- **撤销不能逐项复原。** 约一半的对象多了"自动继承"标志，权限本身不变，无害。但 **pnpm 的硬链接文件权限被改了，撤销改不回来**：`node_modules` 里的文件和 pnpm 全局仓库里的是同一个文件，登记时权限往下传，仓库里的文件也带上了结界的写权限，继承来的其他权限也按洞府重算了。换到真实环境，就是结界里能改所有项目共用的依赖文件，撤销后它们的权限也回不到原样。测试用的是临时的私有 pnpm 仓库，没有碰真仓库。

### 所以

写受限这条路（Codex 免管理员版，以及 deepseek）在 Node 项目里不实用：vitest 这类靠子进程管道的工具一律失败，分身只能逐次请示出结界。

纯低完整性是唯一跑通的免管理员做法，还顺带堵住了 Everyone 可写的口子。它的代价：

- 洞府要打"低"标签。同一用户下别的低完整性进程从此也能写这处洞府。
- 打标签要"取得所有权"的权限。这台机器上 `D:\` 根下新建的目录默认没有，得先给自己加完全控制。
- 标签同样会顺着硬链接改到 pnpm 仓库，那样别的低完整性进程也能改仓库文件，比写受限的后果更大。登记时得自己遍历目录树，跳过链接数大于 1 的文件。
- 低完整性进程写不了大多数注册表项，跟别的程序打交道也受限。这些这次没测。
- 去掉标签是不是真能复原，这次也没测。

几种做法都不受影响的：PowerShell 要用非商店版，`TEMP` 要指到私有目录，`pnpm install` 要不要放行 pnpm 的仓库得另外定。

## 两家怎么做

两家的底子相同：`CreateRestrictedToken` 加 `WRITE_RESTRICTED`。这种令牌的写类访问要查两遍，用户本来的身份查一遍，"限制名单"里的 SID 再查一遍，都过才放行；读不受影响。再造一个不对应任何账号的"能力 SID"放进名单，并在洞府上给它加允许项，于是只有洞府可写。

差别在名单之外怎么兜底：

| | Codex（免管理员模式） | deepseek |
|---|---|---|
| 令牌标志 | `DISABLE_MAX_PRIVILEGE \| LUA_TOKEN \| WRITE_RESTRICTED` | `DISABLE_MAX_PRIVILEGE \| WRITE_RESTRICTED` |
| 限制名单 | 能力 SID、登录会话 SID、Everyone | 同左 |
| 完整性级别 | 不动 | 令牌降到"低"，洞府打"低"标签 |
| 能力 SID | 随机生成后存盘，每个可写目录一个 | 由路径哈希派生，洞府一个、临时目录一个 |
| 洞府上的允许项 | 读、写、执行、删除；**不给"删除子项"** | 写、删除、删除子项 |
| 防"靠父目录权限删文件" | 不给"删除子项"，改为每个后代各自有删除权 | 对 Everyone 加一条拒绝"删除子项" |
| 洞府内的保护 | 对 `.git`、`.codex`、`.agents`、`.aws` 加拒绝写 | 没有 |
| 权限项 | 留着不撤 | 洞府的留着，临时目录的用完撤 |
| 临时目录 | 把用户的 TEMP 当可写目录 | 每次一个私有目录，用完删 |
| 进程约束 | 作业对象、私有桌面、只传三个标准句柄 | 作业对象 |

**为什么 deepseek 要加完整性标签**：Everyone 在限制名单里，所以凡是权限里写着"Everyone 可写"的地方，两遍检查都能过。deepseek 把令牌降到低完整性，没打"低"标签的对象一律写不了，堵住了这个口子。Codex 没堵（推断：它的源码里找不到任何完整性级别的设置，也没有扫描这类目录的代码）。

**标签的代价**：标签是永久的，`icacls` 撤不掉；同一用户下任何低完整性进程从此都能写这处洞府；WMI、`Get-ComputerInfo` 在结界里用不了；别的沙箱工具动过的文件可能读不了。

Codex 另有两套我们暂时不学的：

- **管理员安装版**：建 `CodexSandboxOffline`、`CodexSandboxOnline` 两个本地账号，命令用它们的身份跑，再叠同样的受限令牌。读限制来自"换了账号本来什么都读不了，再逐个目录开放只读"，跳过 `.ssh`、`.gnupg`、`.aws`、`.kube` 等；联网限制来自按离线账号 SID 建的防火墙规则。要 UAC 一次，要签名的安装程序，密码用 DPAPI 存盘。
- **mxc**：微软 MXC SDK 的进程容器，不改权限、不建账号、不要管理员。还在开发中，默认关，要系统带相应接口。

## 第一版的做法

### 令牌

照 Codex：

1. 复制当前进程的令牌。
2. `CreateRestrictedToken`，标志 `DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED`，限制名单依次是：各可写目录的能力 SID、登录会话 SID、Everyone。
3. 重新启用 `SeChangeNotifyPrivilege`（否则遍历目录会失败）。
4. 令牌的默认权限设成：登录会话 SID 完全控制，OWNER RIGHTS（`S-1-3-4`）只读权限。这样它新建的对象不会带上能力 SID，别的结界里的进程碰不到。

任何一步失败都报错，绝不退回成不受限地运行。

### 能力 SID

每个可写目录一个（Codex 的做法）：洞府一个、私有临时目录一个、每个另填的目录各一个。这样以前给某个目录留下的权限项，不会让后来的结界多出权限。

SID 由目录的规范路径派生（deepseek 的做法）：SHA-256 取前 8 字节，拼成 `S-1-4-<x>-<y>`。不用存盘，重复登记自然幂等。它不是秘密，权力只来自写着它的权限项。

### 目录上的改动

对每个可写目录调一次 `SetNamedSecurityInfoW`，加一条允许项：

- 给谁：该目录的能力 SID。
- 允许什么：`FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE`。不给 `FILE_DELETE_CHILD`。
- 继承：子目录和文件（`OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE`）。

登记前先读现有权限，已有同样的项就跳过，所以只有第一次慢（要传播到整棵树）。读改写全程持一把按路径的锁文件。

不打完整性标签，不加拒绝项。撤销就是删掉这个 SID 的项。

可写目录有哪些，和另外两个系统保持一致：

| 目录 | 说明 |
|---|---|
| 洞府 | 独立工作树的分身是它的工作树 |
| 仓库的 `.git` 目录 | 工作树的历史存在原仓库里，不登记它就提交不了 |
| 私有临时目录 | `~/.odg/tmp/<洞府哈希>`，`TEMP`、`TMP` 指过去。不登记用户自己的 TEMP：那棵树大，而且等于对所有结界开放 |
| 设置里"另外准许写入的目录" | 每个一个 SID；第一次登记要遍历那棵树，在设置页写明 |

洞府里的 `.git` 第一版不单独保护，和 Linux、macOS 一致（那边也没有，路线图里记着）。Codex 的"拒绝写 `.git`"会让结界里的 `git commit` 失败，要保护应当只护 `.git/hooks` 和 `.git/config`，三个系统一起做。

### 起进程

`CreateProcessAsUserW` 用上面的令牌起 shell，同时：

- 放进一个 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的作业对象，用 `PROC_THREAD_ATTRIBUTE_JOB_LIST` 在创建时就挂上，没有"先起后挂"的空档。
- 用 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 只传标准输入输出三个句柄。

私有桌面先不做。

## 已知的代价和漏洞

要写进设置页和 README：

- **只管写和删。** 读文件、联网不受限。
- **Everyone 可写的地方挡不住**（推断）。常见的有 `C:\Users\Public`、某些软件装在 `C:\` 根下的目录、`C:\ProgramData` 下的个别子目录。
- **硬链接是漏洞。** 洞府里一个文件若在洞府外还有硬链接，外面那个也跟着可写。pnpm 的仓库大量用硬链接，所以不能拒绝硬链接。两家都没解决。
- **权限项是留着的。** 我们提供撤销；洞府改名后旧的项成了残留（无害，没有令牌再带那个 SID）。
- **第一次登记要遍历整棵目录树。** deepseek 的记录是大洞府几十秒。
- **洞府要归当前用户所有。** 没有改权限的权利就登记不了；FAT 卷不行。
- **Codex 的冒烟测试里有一条标着"已知失败"**：结界里 `Start-Process https://…` 能把网址交给外面的浏览器打开。我们的放行规则本来就不放行 `Start-Process`，但护法或用户准了它，结界拦不住。

## 搬到这里

### 形状

和 macOS 一样在外面套一个启动器，[tools.rs](../crates/odg-core/src/tools.rs) 里跑命令、读输出、超时、后台任务那一套都不用动：

```
odg（界面或命令行）
  └─ odg-sandbox-run.exe --root <洞府> --root <临时目录> … -- pwsh.exe -Command …
       └─ pwsh.exe（受限令牌，在作业对象里）
```

启动器造令牌、起进程、把自己的标准输入输出原样传下去、等它结束、带回退出码。它不改任何权限：登记由 odg 主进程在第一次需要时做，因为它慢、要请示、只做一次。

启动器被杀时作业对象关闭，整棵进程树跟着结束，比现在的 `taskkill /T` 可靠。

### 要改的地方

| 文件 | 改动 |
|---|---|
| `crates/odg-core/src/sandbox/windows.rs`（新） | 派生 SID、登记和撤销、造令牌、起进程。用 `windows-sys`（已在依赖树里）。文件头注明参照的两家 |
| `crates/odg-core/src/bin/odg-sandbox-run.rs`（新） | 启动器入口。失败时打印 `odg-sandbox-run: <原因>` 并以 127 退出 |
| [sandbox.rs](../crates/odg-core/src/sandbox.rs) | `support()` 在 Windows 上返回"受限令牌"或原因；`command()` 返回启动器的命令行 |
| [tools.rs](../crates/odg-core/src/tools.rs) | 认出 127 加 `odg-sandbox-run:` 是结界坏了，而不是命令失败；窗口标志按验证结果定 |
| [policy.rs](../crates/odg-core/src/policy.rs)、[agent.rs](../crates/odg-core/src/agent.rs) | 新增一条请示：第一次在某个目录布结界前问一次"要给这个文件夹加结界标记"。准了记成通行符 |
| [Transcript.tsx](../src/components/Transcript.tsx)、[Settings.tsx](../src/components/Settings.tsx) | 这条请示的文案；设置页列出登记过的目录，各带"撤销标记"；写明上面的代价 |
| `src-tauri/tauri.conf.json` | 把启动器作为附带的可执行文件一起打包 |

### 测试

- 派生 SID、拼命令行：单元测试，三个系统都跑。
- 真结界：`crates/odg-core/tests/sandbox_windows.rs`，用 `CARGO_BIN_EXE_odg-sandbox-run` 找启动器。内容和 Linux 上那条一样：洞府内能建、改、移、删；洞府外不能写、不能删、不能移出去；嵌套子进程也一样；两处洞府互相写不了。本机和 CI 的 Windows 任务都能真跑。
- 失败即拒绝：造令牌或登记的任一步失败时，命令没有被执行。
- 撤销之后，目录的权限和登记之前逐项相同。
- 端到端里"结界开关是禁用的"那条检查改成相反的，并加一段真实的结界内命令。

## 分步

每一步都能单独验证，前一步不过就不往下走。

**第一步：验证（一两天，不进主分支）。** 2026-11-19 已做，第一问没过，结果见上面"第一步的验证结果"。写一个最小的启动器，手工登记一个临时目录，在这台机器上回答：

1. `cargo build`、`cargo test`、`git commit`、`pnpm install`、`pnpm test`、`pnpm build` 在结界里能不能跑通？尤其是 Node 用管道起子进程的那些。**这一问决定值不值得做**：大面积失败就停在这里，把结论记进路线图。
2. 从没有控制台的界面进程起启动器，再起 `pwsh`，会不会弹窗口、会不会在加载 DLL 时失败？deepseek 的记录是带 `CREATE_NO_WINDOW` 会以 `0xC0000142` 失败，但那是降了完整性的令牌，我们不降，要重测。
3. `> $null`、`2>&1`、`> NUL` 这些重定向在结界里好不好用？Codex 专门给空设备授权，deepseek 说名单里有 Everyone 就够。
4. 找一个 Everyone 可写的目录，确认结界里确实能写，把这条漏洞从"推断"变成"实测"。
5. 给这个仓库（含 `target` 和 `node_modules`）做第一次登记要多久？撤销后权限是否复原？

**第二步：核心。** `windows.rs`、启动器、`support()` 和 `command()`，加集成测试。此时没有请示，只在测试里用临时目录。

**第三步：请示和撤销。** 首次登记前的请示、设置页里的撤销和说明、失败时给分身的提示（沿用现在的"可以请示出结界"）。

**第四步：打包和端到端。**

## 以后

按需要再做，互不依赖：

- **堵 Everyone 可写的口子**：deepseek 的低完整性标签。做成设置里的一个"加固"开关，默认关，因为标签撤不干净、副作用多。
- **管读和联网**：Codex 的管理员安装版。要先有签名和安装程序。
- **mxc**：等它不再是开发中，又不改用户的权限，可能是最干净的一条路。
- **`.git/hooks` 和 `.git/config` 的保护**：三个系统一起做。

## 没把握的地方

- 两家文档和源码里的每一条我都没复现过。第一步就是为了这个。
- "Everyone 可写的地方挡不住"是从 Codex 的令牌构造推出来的，它自己没写。
- Codex 是控制台程序，我们是窗口程序。它不会遇到的窗口和控制台问题，我们可能遇到。
- 登记是永久改动用户的文件夹。有请示和撤销，但这是我们第一次改洞府内容以外的东西。deepseek 为此专门写了一个四万多字节的诊断修复脚本，可见容易出事；我们不打标签，面要小得多，但不是零。

## 来源

Codex（`codex-rs/windows-sandbox-rs/src/`）：

- `token.rs` 的 `create_token_with_caps_from`：令牌标志、限制名单的顺序、默认权限。
- `acl.rs` 的 `WRITE_ALLOW_MASK` 及其注释：不给"删除子项"的理由；`allow_null_device`、`add_deny_write_ace`。
- `cap.rs`：能力 SID 的生成和存放。
- `lib.rs` 的 `run_windows_sandbox_capture_with_filesystem_overrides`：免管理员模式的入口。
- `sandbox_smoketests.py`：它期望拦住和放行的行为。

deepseek（`packages/sandbox/sandbox-windows-acl/`）：

- `README.md`：机制、已验证的边界、已知限制。
- `src/workspace-sid.ts`：由路径派生 SID。
- `src/token.ts`、`src/acl.ts`：令牌和权限项。
- `.agents/notes/implemented/feature/2026-08-08-windows-acl-restricted-token-sandbox.md`：为什么选这条路。

实现时照它们写明的机制用 Rust 重写，不照抄代码。
