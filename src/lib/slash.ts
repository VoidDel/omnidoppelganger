// The "/" commands: typed at the start of the message box, they act on the 分身 or the window instead of being
// sent to it. What is typed after "/" picks from a menu; a command named, a space after it, offers the values its
// argument may take. Text that names no command is sent as a message, as "/usr/bin is missing" should be.

/** A value a command's argument may take: what is typed, what it is called, and what it does. */
export interface SlashChoice {
  value: string;
  label: string;
  hint?: string;
}

export interface SlashCommand {
  /** What follows "/": letters, digits, "-" and "_", typed without switching the input method. */
  name: string;
  /** What it is called in the user's language, shown beside the name. */
  label: string;
  /** Words it is also found by, in the user's language. */
  aliases: string[];
  /** What it does, in a line. */
  hint: string;
  /** What is written after the name, as the menu shows it, when it takes something. */
  arg?: string;
  /** The values its argument may take, offered once its name is typed. */
  choices?: SlashChoice[];
  /** Why it cannot be used just now, when it cannot. */
  unavailable?: string;
  /** Do it, with what was written after the name, trimmed. Gives back why it was not done, when it was not. */
  run: (arg: string) => string | void;
}

/** The text read as a command: the name after "/", what follows it, and whether the name is finished by a space. */
export function parse(text: string): { name: string; arg: string; named: boolean } | null {
  if (!text.startsWith("/") || text.startsWith("//")) return null;
  const rest = text.slice(1);
  const space = rest.search(/\s/);
  if (space < 0) return { name: rest, arg: "", named: false };
  return { name: rest.slice(0, space), arg: rest.slice(space + 1).trim(), named: true };
}

/** The command a name or alias stands for. */
export function find(commands: SlashCommand[], name: string): SlashCommand | undefined {
  const lower = name.toLowerCase();
  return commands.find((command) => command.name.toLowerCase() === lower) ?? commands.find((command) => command.aliases.includes(name));
}

/** The value of `command`'s argument that what was written stands for: by its value, or by what it is called. */
export function choose(command: SlashCommand, arg: string): SlashChoice | undefined {
  const lower = arg.toLowerCase();
  return command.choices?.find((choice) => choice.value.toLowerCase() === lower || choice.label === arg);
}

export type Offer = { kind: "command"; command: SlashCommand } | { kind: "choice"; command: SlashCommand; choice: SlashChoice };

/**
 * What the menu offers for the text in the box. While the name is being typed: the commands whose name or alias
 * begins with it. Once it is finished: that command's values that begin with what follows, or, for a command that
 * takes free words, the command itself, to show what it will do with them.
 */
export function offers(commands: SlashCommand[], text: string): Offer[] {
  const typed = parse(text);
  if (!typed) return [];
  if (!typed.named) {
    const lower = typed.name.toLowerCase();
    return commands
      .filter((command) => command.name.toLowerCase().startsWith(lower) || (typed.name && command.aliases.some((alias) => alias.startsWith(typed.name))))
      .map((command) => ({ kind: "command", command }));
  }
  const command = find(commands, typed.name);
  if (!command) return [];
  if (!command.choices) return [{ kind: "command", command }];
  const lower = typed.arg.toLowerCase();
  return command.choices
    .filter((choice) => choice.value.toLowerCase().startsWith(lower) || choice.label.startsWith(typed.arg))
    .map((choice) => ({ kind: "choice", command, choice }));
}

/** What the box holds once an offer is taken without being done: the command named and a space, or the value written in. */
export function completion(offer: Offer): string {
  if (offer.kind === "choice") return `/${offer.command.name} ${offer.choice.value}`;
  return offer.command.arg || offer.command.choices ? `/${offer.command.name} ` : `/${offer.command.name}`;
}
