import type { CliBackend, Mode } from "./types";

/** The four restrictions (禁制) a 分身 works under, in the order Shift+Tab cycles them. */
export const MODES: { value: Mode; name: string; hint: string }[] = [
  { value: "manual", name: "步步请示", hint: "改文件、跑命令，每一步都先请本尊定夺" },
  { value: "edit", name: "府内自便", hint: "洞府之内的文件读写自便，跑命令和碰洞府之外的仍要请示" },
  { value: "plan", name: "先谋后动", hint: "只看不改，摸清之后呈上计划，本尊点头再动手" },
  { value: "auto", name: "放手施为", hint: "只读、构建、测试类命令自行放行，有风险的才停下请示" },
];

/**
 * The fifth, which asks about nothing. It is switched on by hand in one session with a worktree of its own, after
 * saying what it means: never a default, never a schedule's, never reached by Shift+Tab.
 */
export const BYPASS = { value: "bypass" as Mode, name: "全权托付", hint: "什么都不请示：任何命令、洞府之外、.git、上网、身外法器，一概自行放行" };

export const modeOf = (value: Mode | undefined) => (value === "bypass" ? BYPASS : (MODES.find((mode) => mode.value === value) ?? MODES[1]));

/** The mode after `value`, for Shift+Tab: one of the four, so it never lands on 全权托付; from that, the first. */
export const nextMode = (value: Mode): Mode => MODES[(MODES.findIndex((mode) => mode.value === value) + 1) % MODES.length].value;

/**
 * Describe what `mode` actually enforces when a session is backed by a headless external CLI (`cli !== null`),
 * where per-step interactive approval cards do not exist.
 */
export function modeHintFor(mode: Mode | undefined, cli: CliBackend | null = null): string {
  const resolved = modeOf(mode);
  if (!cli) return resolved.hint;
  switch (resolved.value) {
    case "manual":
      return "无头 CLI 不弹逐条审批卡，此档下只读不改、不跑命令";
    case "plan":
      return "只读分析与规划，不改文件、不跑写入命令";
    case "edit":
      return cli === "codex"
        ? "Codex 无头模式下按洞府内可写沙箱（workspace-write）运行"
        : "允许改洞府内文件，不自动放行命令（需跑命令请切至「放手施为」）";
    case "auto":
      return "交由本机 CLI 的自动/审核模式放行命令与已启用的 MCP 法器";
    case "bypass":
      return "全权放行：跳过本机 CLI 的全部权限询问";
  }
}
