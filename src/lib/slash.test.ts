import { describe, expect, it } from "vitest";
import { choose, completion, find, offers, parse, type SlashCommand } from "./slash";

const command = (name: string, extra: Partial<SlashCommand> = {}): SlashCommand => ({ name, aliases: [], hint: "", run: () => undefined, ...extra });
const commands = [
  command("compact", { aliases: ["凝练", "摘要"] }),
  command("effort", { aliases: ["境界"], arg: "<境界>", choices: [{ value: "low", label: "练气" }, { value: "high", label: "金丹" }] }),
  command("rename", { aliases: ["改名"], arg: "<新名字>" }),
  command("review"),
];
const names = (text: string) => offers(commands, text).map((offer) => (offer.kind === "choice" ? offer.choice.value : offer.command.name));

describe("the / commands", () => {
  it("read what is typed as a command only when it starts the box with a single /", () => {
    expect(parse("/rename 新的名字 还有")).toEqual({ name: "rename", arg: "新的名字 还有", named: true });
    expect(parse("/re")).toEqual({ name: "re", arg: "", named: false });
    expect(parse("看看 /usr/bin")).toBeNull();
    expect(parse("// 注释")).toBeNull();
  });

  it("offer the commands a name or an alias begins, then the values of the one named", () => {
    expect(names("/")).toEqual(["compact", "effort", "rename", "review"]);
    expect(names("/re")).toEqual(["rename", "review"]);
    expect(names("/摘")).toEqual(["compact"]);
    expect(names("/effort ")).toEqual(["low", "high"]);
    expect(names("/effort 金")).toEqual(["high"]);
    // A command of free words shows itself while they are written; one that is not a command offers nothing.
    expect(names("/rename 新")).toEqual(["rename"]);
    expect(names("/usr/bin 不见了")).toEqual([]);
  });

  it("find a command by its name or alias, and a value by itself or by what it is called", () => {
    expect(find(commands, "COMPACT")?.name).toBe("compact");
    expect(find(commands, "境界")?.name).toBe("effort");
    const effort = find(commands, "effort")!;
    expect([choose(effort, "LOW")?.value, choose(effort, "金丹")?.value, choose(effort, "化神")]).toEqual(["low", "high", undefined]);
  });

  it("complete a command that takes something with a space, and a value in full", () => {
    const [compact, effort, rename] = commands;
    expect(completion({ kind: "command", command: compact })).toBe("/compact");
    expect(completion({ kind: "command", command: rename })).toBe("/rename ");
    expect(completion({ kind: "choice", command: effort, choice: effort.choices![1] })).toBe("/effort high");
  });
});
