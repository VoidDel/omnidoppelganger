import type { Mode } from "./types";

/** The four restrictions (禁制) a 分身 works under, in the order Shift+Tab cycles them. */
export const MODES: { value: Mode; name: string; hint: string }[] = [
  { value: "manual", name: "步步请示", hint: "改文件、跑命令，每一步都先请本尊定夺" },
  { value: "edit", name: "府内自便", hint: "洞府之内的文件读写自便，跑命令和碰洞府之外的仍要请示" },
  { value: "plan", name: "先谋后动", hint: "只看不改，摸清之后呈上计划，本尊点头再动手" },
  { value: "auto", name: "放手施为", hint: "只读、构建、测试类命令自行放行，有风险的才停下请示" },
];

export const modeOf = (value: Mode | undefined) => MODES.find((mode) => mode.value === value) ?? MODES[1];

/** The mode after `value`, for Shift+Tab. */
export const nextMode = (value: Mode): Mode => MODES[(MODES.findIndex((mode) => mode.value === value) + 1) % MODES.length].value;
