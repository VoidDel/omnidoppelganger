import { type CSSProperties, useEffect, useLayoutEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import { EFFORT_HINTS, EFFORT_NAMES, effortNote } from "../lib/efforts";
import { launchFrom } from "../lib/flight";
import { type Usage, documentLabel, fileSize, shortCount } from "../lib/fold";
import { MODES, nextMode } from "../lib/modes";
import { CLI_INFO, cliOf } from "../lib/providers";
import { type Offer, type SlashChoice, type SlashCommand, completion, offers, parse } from "../lib/slash";
import type { Image, Mode, Picked, Provider, Status } from "../lib/types";
import { Mark } from "./Backdrop";
import { Light } from "./Light";
import { CHECK, LIMITS, ModePicker, ModelList, Popup } from "./Pickers";
import { Viewer, imageSource } from "./Viewer";

// What a message may carry, as the core checks it: these formats, this many, this large.
const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];
const IMAGES_PER_MESSAGE = 8;
const FILES_PER_MESSAGE = 10;

/** A file waiting to be sent with the message. */
export type Pending = Extract<Picked, { kind: "file" }>;

export interface ComposerDraft {
  text: string;
  images: Image[];
  files: Pending[];
}

/** Why a path could not be attached, in the user's words. */
const REFUSALS: Record<Extract<Picked, { kind: "refused" }>["why"], (name: string) => string> = {
  missing: (name) => `「${name}」读不到，可能已经不在了。`,
  directory: (name) => `「${name}」是个文件夹，只能附文件。要分身看整个文件夹，直接把路径告诉它。`,
  binary: (name) => `「${name}」不是文本文件，也不是 PDF、Word、Excel、PPT 这类文档，附不了。`,
  too_large: (name) => `「${name}」太大了，分身读不了：文本文件最多 10 MB，文档最多 50 MB。`,
  image_too_large: (name) => `「${name}」这张图超过 3.7 MB，太大了，缩小或截小一点再附。`,
  doc_unreadable: (name) => `「${name}」读不出来，可能已经损坏或不完整。`,
  doc_locked: (name) => `「${name}」设了打开密码，分身读不了。去掉密码再附。`,
  doc_no_text: (name) => `「${name}」里没有文字，多半是扫描件或整页是图，分身读不了。可以截图附上。`,
  doc_old_format: (name) => `「${name}」是老版本的 Word 或 PPT 格式，分身读不了。用 Office 另存为 .docx 或 .pptx 再附。`,
};

/** A sheet of paper with its corner turned: a file. */
const FILE_ICON = (
  <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
    <path d="M4 1.8h5.2L12 4.6v9.6H4zM9.2 1.8v2.8H12M6 8h4M6 10.6h4" />
  </svg>
);
// 5 MB of base64 is 3.75 MB of file.
const IMAGE_BYTES = 3_700_000;

/** A file's bytes in base64, without the `data:` prefix a reader puts in front. */
const readBase64 = (file: File) =>
  new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result).replace(/^data:[^,]*,/, ""));
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });

export interface ComposerProps {
  status: Status;
  /** Every provider this build knows, whether each is in the list the user keeps, and whether each has a key: the 心法 menu offers those in the list. */
  providers: Provider[];
  listed: (provider: string) => boolean;
  keyed: (provider: string) => boolean;
  /** The id of the provider the session is on, which this build may no longer list. */
  providerId: string;
  /** The id of the model the session is on. */
  model: string;
  /** The realm the session works at, whether it was set for it (else it follows the settings), and those there are. */
  effort: string;
  effortSet: boolean;
  efforts: string[];
  onEffort: (effort: string) => void;
  /** Whether the model takes pictures; null when that is not known, in which case they are sent. */
  vision: boolean | null;
  mode: Mode;
  usage: Usage;
  /** The number of tokens at which the conversation is compacted, the most it can be for this model, and the limit as set. */
  contextLimit: number;
  contextCeiling: number;
  contextSetting: number;
  /** Whether the limit follows each model's own window. */
  contextAuto: boolean;
  /** Set the limit, for every session. */
  onLimit: (limit: number | "auto") => void;
  /** Have the 分身 summarize the conversation now. */
  onCompact: () => void;
  onModel: (provider: string, model: string) => void;
  listModels: (provider: string) => Promise<[string, string][]>;
  /** The last turn in plan mode ended with the 分身's answer: offer to carry it out. */
  planReady: boolean;
  onMode: (mode: Mode) => void;
  /** Switch to `mode`, then tell the 分身 to carry out its plan. */
  onCarryOut: (mode: Mode) => void;
  /** `files`: the paths of the files attached. */
  onSend: (text: string, images: Image[], files: string[]) => void;
  onStop: () => void;
  /** Whether the session works in a worktree of its own, the only place 全权托付 may be switched on. */
  trustable: boolean;
  /** Session id + shared draft map so moving between center chat and side chat keeps typed words and attachments intact. */
  sessionKey?: string;
  drafts?: Map<string, ComposerDraft>;
  /** Words or file paths to put in the box (from fork, file tree attach, or code viewer quote); a new `id` applies it. */
  seed: { id: number; text?: string; append?: boolean; attachPaths?: string[] } | null;
  /** The "/" commands the window gives, beside the box's own (心法, 境界, 禁制, 凝练); skills among them. */
  commands: SlashCommand[];
  /** What the empty box says, in place of the main conversation's own words. */
  placeholder?: string;
  /** Whether files dragged onto the window attach to this box (defaults to true). */
  acceptDrops?: boolean;
}

/** The order the "/" menu lists the commands in; any not named here (a skill's) come after, as given. */
const COMMAND_ORDER = ["new", "aside", "fork", "compact", "model", "effort", "mode", "files", "git", "terminal", "rename", "replay", "init", "review", "skills", "mcp", "schedule", "settings"];

/**
 * How full the conversation is, as five 灵石 lit one after another: each stands for a fifth of the room there is
 * before it is summarized. They are jade, turn gold past seven tenths, and cinnabar past nine, where the last one beats.
 */
function Stones({ part }: { part: number }) {
  const full = Math.min(Math.max(part, 0), 1);
  return (
    <span className={`stones ${full >= 0.9 ? "hot" : full >= 0.7 ? "warm" : ""}`} aria-hidden="true">
      {[0, 1, 2, 3, 4].map((at) => (
        <span key={at} style={{ "--lit": Math.min(Math.max(full * 5 - at, 0), 1) } as CSSProperties} />
      ))}
    </span>
  );
}

/** How tall the box to type in stands with nothing in it, in pixels: as it starts, the least, and where it is kept. */
const BOX = { initial: 54, least: 28, key: "odg.composer" };
/** The most it may be made, or grow to with what is typed: it leaves the conversation most of the window. */
const tallest = () => Math.max(BOX.initial, Math.round(window.innerHeight * 0.55));
const fitBox = (height: number) => Math.round(Math.min(Math.max(height, BOX.least), tallest()));

/** The height the box was last left at. */
function useBoxHeight() {
  const [height, setHeight] = useState(() => {
    try {
      const kept = Number(localStorage.getItem(BOX.key));
      return kept > 0 ? fitBox(kept) : BOX.initial;
    } catch {
      return BOX.initial;
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(BOX.key, String(height));
    } catch {
      // Storage may be unavailable; the box then starts out as it does the first time.
    }
  }, [height]);
  return [height, setHeight] as const;
}

export function Composer(props: ComposerProps) {
  const { status, providers, listed, keyed, providerId, model, effort, effortSet, efforts, vision, mode, usage, contextLimit, contextCeiling, contextSetting, contextAuto, planReady } = props;
  const { onLimit, onCompact, onModel, onEffort, listModels, onMode, onCarryOut, onSend, onStop, seed, trustable, sessionKey, drafts, commands: given, placeholder, acceptDrops = true } = props;
  const provider = providers.find((known) => known.id === providerId);
  const cli = cliOf(provider);
  const providerName = provider?.name ?? providerId;
  const limits = [...LIMITS.filter((limit) => limit < contextCeiling), contextCeiling];
  const savedDraft = sessionKey ? drafts?.get(sessionKey) : undefined;
  const [text, setText] = useState(() => savedDraft?.text ?? "");
  const input = useRef<HTMLTextAreaElement>(null);
  const [height, setHeight] = useBoxHeight();
  const [dragging, setDragging] = useState(false);
  /** Where a drag of the box's top edge began: pointer Y, visible height, content floor, and saved height. */
  const held = useRef<{ y: number; height: number; floor: number; saved: number; grown: boolean } | null>(null);
  // The box is as tall as it was left, and taller while what is typed needs more, up to what leaves the conversation room.
  useLayoutEffect(() => {
    const box = input.current;
    if (!box) return;
    box.style.height = `${height}px`;
    box.style.height = `${Math.min(Math.max(box.scrollHeight, height), tallest())}px`;
  }, [text, height]);
  const [images, setImages] = useState<Image[]>(() => savedDraft?.images ?? []);
  /** Which of the pictures to go with the message is being looked at full size. */
  const [viewing, setViewing] = useState<number | null>(null);
  /** Files to go with the message, each by its path. */
  const [files, setFiles] = useState<Pending[]>(() => savedDraft?.files ?? []);
  useEffect(() => {
    if (!sessionKey || !drafts) return;
    if (!text && images.length === 0 && files.length === 0) drafts.delete(sessionKey);
    else drafts.set(sessionKey, { text, images, files });
  }, [sessionKey, drafts, text, images, files]);
  /** Why something was not taken, until the next attempt. */
  const [refused, setRefused] = useState<string | null>(null);
  /** Files are being dragged over the window: the box shows it will take them. */
  const [dropping, setDropping] = useState(false);
  const busy = status !== "idle";
  const empty = !text.trim() && images.length === 0 && files.length === 0;
  const sender = useRef<HTMLButtonElement>(null);
  const submit = () => {
    if (empty) return;
    // The token on the button flies to the message's place in the conversation.
    launchFrom(sender.current);
    onSend(
      text,
      images,
      files.map((file) => file.path),
    );
    setText("");
    setImages([]);
    setFiles([]);
    setRefused(null);
  };

  // The "/" commands: the box's own, for what it holds the controls of, and the window's.
  /** Changed to open the 心法 menu, from "/model". */
  const [modelSummon, setModelSummon] = useState(0);
  const effortChoices: SlashChoice[] = efforts.map((level) => ({ value: level, label: EFFORT_NAMES[level] ?? level, hint: EFFORT_HINTS[level] }));
  const modeChoices: SlashChoice[] = MODES.map((one) => ({ value: one.value, label: one.name, hint: one.hint }));
  const picked = (choices: SlashChoice[], arg: string, what: string) =>
    choices.find((choice) => choice.value === arg.toLowerCase() || choice.label === arg) ?? `没有「${arg}」这一${what}：可选 ${choices.map((choice) => choice.label).join("、")}。`;
  const own: SlashCommand[] = [
    {
      name: "compact",
      label: "凝练",
      aliases: ["凝练", "摘要", "压缩"],
      hint: "现在就把前情凝成摘要，之后的请求省灵石",
      unavailable: busy ? "分身运功时不能凝练，等它收功" : usage.context === 0 ? "还没有前情可凝" : undefined,
      run: () => onCompact(),
    },
    { name: "model", label: "心法", aliases: ["心法", "功法", "模型"], hint: "换心法，也能换到别的宗门", unavailable: busy ? "分身运功时不能换心法，等它收功" : undefined, run: () => setModelSummon((count) => count + 1) },
    {
      name: "effort",
      label: "境界",
      aliases: ["境界"],
      hint: "换这个分身的境界：推演深浅",
      arg: "<境界>",
      choices: effortChoices,
      run: (arg) => {
        const choice = picked(effortChoices, arg, "档境界");
        if (typeof choice === "string") return choice;
        onEffort(choice.value);
      },
    },
    {
      name: "mode",
      label: "禁制",
      aliases: ["禁制"],
      hint: "换禁制：分身不问就能做什么",
      arg: "<禁制>",
      choices: modeChoices,
      run: (arg) => {
        const choice = picked(modeChoices, arg, "种禁制");
        if (typeof choice === "string") return choice;
        onMode(choice.value as Mode);
      },
    },
  ];
  const rank = (command: SlashCommand) => (COMMAND_ORDER.includes(command.name) ? COMMAND_ORDER.indexOf(command.name) : COMMAND_ORDER.length);
  const commands = [...own, ...given].sort((a, b) => rank(a) - rank(b));
  /** The text the menu was put away for with Escape: shown again once the text changes. */
  const [dismissed, setDismissed] = useState<string | null>(null);
  /** The menu opened with the "/" button: every command, whatever the box holds, until one is taken or it is put away. */
  const [browsing, setBrowsing] = useState(false);
  const menu = browsing ? offers(commands, "/") : dismissed === text ? [] : offers(commands, text);
  const [chosen, setChosen] = useState(0);
  // What is typed takes over from the button's menu.
  useEffect(() => {
    setChosen(0);
    setBrowsing(false);
  }, [text]);
  const menuList = useRef<HTMLDivElement>(null);
  // Kept in sight as the choice moves. In braces: scrolling now answers with a promise, which an effect must not
  // hand back, or it is taken for the effect's cleanup and called when the choice next changes.
  useEffect(() => {
    menuList.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
  }, [chosen]);
  /**
   * Do a command with what follows it, and empty the box of the command typed there; or say why it was not done.
   * With `keep`, the box is left as it is: the command came from the button, and what the box holds is not it.
   */
  const perform = (command: SlashCommand, arg: string, keep = false) => {
    if (command.unavailable) return setRefused(command.unavailable);
    const why = command.run(arg);
    if (typeof why === "string") return setRefused(why);
    if (!keep) setText("");
    setRefused(null);
  };
  /** Take an offer: a value is done at once; a command is done, unless it wants something yet to be written, which it is completed to wait for. */
  const accept = (offer: Offer) => {
    if (offer.command.unavailable) return setRefused(offer.command.unavailable);
    if (offer.kind === "choice") return perform(offer.command, offer.choice.value);
    // Opened with the button, the menu is not about what the box holds: the command is taken as if "/" alone were typed.
    const typed = browsing ? parse("/") : parse(text);
    const fromButton = browsing;
    setBrowsing(false);
    if ((!typed?.named && (offer.command.arg || offer.command.choices)) || (offer.command.choices && !typed?.arg)) {
      setText(completion(offer));
      input.current?.focus();
      return;
    }
    perform(offer.command, typed?.arg ?? "", fromButton);
  };
  const blind = `${model} 这门心法只认字，看不了图。点左下的心法，换一门能看图的。`;
  /** Attach the pictures pasted into the box, saying why when one cannot be sent. */
  const attach = async (pasted: File[]) => {
    setRefused(null);
    if (vision === false) return setRefused(blind);
    const taken: Image[] = [];
    for (const file of pasted) {
      if (!IMAGE_TYPES.includes(file.type)) setRefused("只能贴 PNG、JPEG、GIF、WebP 格式的图。");
      else if (file.size > IMAGE_BYTES) setRefused("这张图超过 3.7 MB，太大了，缩小或截小一点再附。");
      else if (images.length + taken.length >= IMAGES_PER_MESSAGE) setRefused(`一次最多附 ${IMAGES_PER_MESSAGE} 张图。`);
      else taken.push({ media_type: file.type, data: await readBase64(file) });
    }
    if (taken.length) setImages((current) => [...current, ...taken].slice(0, IMAGES_PER_MESSAGE));
  };
  // What the last render knew, for what comes in between renders: files dropped on the window.
  const known = useRef({ vision, blind });
  known.current = { vision, blind };
  /** Take what was picked or dropped: files by their paths, pictures as pictures; say why of whatever is not taken. */
  const take = (paths: string[]) => {
    if (paths.length === 0) return;
    setRefused(null);
    api.attachFiles(paths).then(
      (picked) => {
        const whys: string[] = [];
        const pictures = picked.filter((one) => one.kind === "image");
        const papers = picked.filter((one) => one.kind === "file");
        for (const one of picked) if (one.kind === "refused") whys.push(REFUSALS[one.why](one.name));
        if (pictures.length > 0 && known.current.vision === false) whys.push(known.current.blind);
        else if (pictures.length > 0)
          setImages((current) => {
            if (current.length + pictures.length > IMAGES_PER_MESSAGE) setRefused(`一次最多附 ${IMAGES_PER_MESSAGE} 张图，多的没有附上。`);
            return [...current, ...pictures.map((one) => one.image)].slice(0, IMAGES_PER_MESSAGE);
          });
        if (papers.length > 0)
          setFiles((current) => {
            // The same file picked twice is attached once.
            const fresh = papers.filter((one, at) => !current.some((had) => had.path === one.path) && papers.findIndex((other) => other.path === one.path) === at);
            if (current.length + fresh.length > FILES_PER_MESSAGE) setRefused(`一次最多附 ${FILES_PER_MESSAGE} 个文件，多的没有附上。`);
            return [...current, ...fresh].slice(0, FILES_PER_MESSAGE);
          });
        if (whys.length > 0) setRefused(whys.join(""));
      },
      (error) => setRefused(`没附上：${String(error)}`),
    );
  };
  const taking = useRef(take);
  taking.current = take;
  const lastSeedId = useRef<number | null>(null);
  useEffect(() => {
    if (!seed || lastSeedId.current === seed.id) return;
    lastSeedId.current = seed.id;
    if (seed.attachPaths && seed.attachPaths.length > 0) {
      taking.current(seed.attachPaths);
    }
    if (seed.text !== undefined) {
      const incoming = seed.text;
      setText((prev) => (seed.append && prev.trim() ? `${prev.trimEnd()}\n\n${incoming}` : incoming));
    }
    requestAnimationFrame(() => input.current?.focus());
  }, [seed]);
  // Files dragged from the system onto the window are attached where they are dropped.
  useEffect(() => {
    if (!acceptDrops) {
      setDropping(false);
      return;
    }
    const stop = api.onFilesDragged(({ over, dropped }) => {
      setDropping(over);
      taking.current(dropped);
    });
    return () => {
      stop.then((unlisten) => unlisten()).catch(() => undefined);
    };
  }, [acceptDrops]);
  return (
    <div className="composer">
      <div className="composer-status" role="status">
        {status === "running" && (
          <>
            <Mark state="working" size={18} />
            <span>分身正在运功。此刻传音，它做完这一步就会听到，不会打断它。</span>
          </>
        )}
        {status === "waiting_approval" && (
          <>
            <Mark state="waiting" size={18} />
            <span>分身收功候旨，本尊定夺后继续。</span>
          </>
        )}
        {mode === "bypass" && status === "idle" && !planReady && (
          <span className="trusted-note">全权托付中：分身什么都不请示。</span>
        )}
        {cli && mode !== "bypass" && status === "idle" && !planReady && (
          <span>
            {CLI_INFO[cli].label}（无头流式模式）：工具执行与会话上下文由本机 {CLI_INFO[cli].bin} 托管。
          </span>
        )}
        {status === "idle" && planReady && (
          <>
            <span>计划已呈上。本尊点头，就换个禁制让它动手：</span>
            {MODES.filter((option) => option.value === "edit" || option.value === "auto").map((option) => (
              <button type="button" key={option.value} className="button outline small" onClick={() => onCarryOut(option.value)}>
                {option.name}，照计划动手
              </button>
            ))}
          </>
        )}
      </div>
      {/* The frame carries the light round the box: jade and gold going round while the 分身 works, cinnabar held while it waits. */}
      <div className={`composer-frame ${status === "running" ? "working" : status === "waiting_approval" ? "waiting" : ""}`}>
        {/* Not in 全权托付: there the box keeps its cinnabar edge, which says more. */}
        {status === "running" && mode !== "bypass" && <Light aura />}
        <div className={`composer-box ${mode === "bypass" ? "trusted" : ""} ${dropping ? "dropping" : ""}`}>
          {dropping && <div className="drop-note">松手，把文件附给分身</div>}
          {/* The box's top edge, to drag: up makes more room to write in, down gives it back to the conversation. */}
          <div
            className={`composer-grip ${dragging ? "dragging" : ""}`}
            role="separator"
            aria-orientation="horizontal"
            aria-label="拖动调整输入框高度，双击恢复"
            aria-valuenow={height}
            tabIndex={0}
            onPointerDown={(event) => {
              event.currentTarget.setPointerCapture(event.pointerId);
              const box = input.current;
              const current = box?.offsetHeight ?? height;
              let content = BOX.least;
              if (box && text.length > 0) {
                const prev = box.style.height;
                box.style.height = "0px";
                content = box.scrollHeight;
                box.style.height = prev;
              }
              const floor = Math.max(BOX.least, Math.min(content, tallest()));
              // From the height the box is seen at, which is more than the one it was left at while what is typed needs more.
              held.current = { y: event.clientY, height: current, floor, saved: height, grown: current > height };
              setDragging(true);
            }}
            onPointerMove={(event) => {
              if (!held.current) return;
              const raw = held.current.height + held.current.y - event.clientY;
              // A box grown by what is typed in it cannot be dragged smaller than that: keep the saved height unchanged.
              if (held.current.grown && raw < held.current.height) {
                setHeight(held.current.saved);
                return;
              }
              setHeight(fitBox(Math.max(held.current.floor, raw)));
            }}
            onPointerUp={() => ((held.current = null), setDragging(false))}
            onPointerCancel={() => ((held.current = null), setDragging(false))}
            onDoubleClick={() => setHeight(BOX.initial)}
            onKeyDown={(event) => {
              if (event.key === "ArrowUp") setHeight(fitBox(height + 24));
              if (event.key === "ArrowDown") setHeight(fitBox(height - 24));
            }}
          />
          {viewing !== null && images.length > 0 && <Viewer images={images} at={viewing} whose="要附的" onClose={() => setViewing(null)} />}
          {(images.length > 0 || files.length > 0 || refused) && (
            <div className="attachments">
              {images.map((image, index) => (
                <span className="attachment" key={index}>
                  <button type="button" className="shot" title="点开看大图" onClick={() => setViewing(index)}>
                    <img src={imageSource(image)} alt={`要附的第 ${index + 1} 张图`} />
                  </button>
                  <button type="button" aria-label={`去掉第 ${index + 1} 张图`} onClick={() => setImages(images.filter((_, at) => at !== index))}>
                    ×
                  </button>
                </span>
              ))}
              {files.map((file) => (
                <span
                  className="attachment file"
                  key={file.path}
                  title={`${file.path}\n${file.inline ? (file.document ? "取出的文字会随这句话一起发给分身，图和排版不在其中" : "它的内容会随这句话一起发给分身") : "它太大，不随消息发送：分身会自己分段去读，不用请示"}`}
                >
                  {FILE_ICON}
                  <span className="file-chip-name">{file.name}</span>
                  <span className="file-chip-size">
                    {fileSize(file.bytes)}
                    {file.document ? `，${documentLabel(file.document, file.parts)}` : ""}
                    {!file.inline && "，分身自读"}
                  </span>
                  <button type="button" aria-label={`去掉 ${file.name}`} onClick={() => setFiles(files.filter((had) => had.path !== file.path))}>
                    ×
                  </button>
                </span>
              ))}
              {refused && (
                <span className="refused" role="alert">
                  {refused}
                </span>
              )}
            </div>
          )}
          {/* The "/" menu: what the typed name may be, or, a command named, the values it takes. */}
          {menu.length > 0 && (
            <div className="slash-menu" id="slash-menu" role="listbox" aria-label="命令" ref={menuList}>
              {menu[0].kind === "choice" && <div className="mode-menu-head">{`/${menu[0].command.name}：${menu[0].command.hint}`}</div>}
              {menu.map((offer, at) => (
                <div
                  key={offer.kind === "choice" ? offer.choice.value : offer.command.name}
                  id={`slash-${at}`}
                  role="option"
                  aria-selected={at === Math.min(chosen, menu.length - 1)}
                  aria-disabled={!!offer.command.unavailable}
                  className="slash-option"
                  title={offer.command.unavailable}
                  onMouseEnter={() => setChosen(at)}
                  // Taken on the press, so that the box keeps the focus the menu was opened from.
                  onMouseDown={(event) => {
                    event.preventDefault();
                    accept(offer);
                  }}
                >
                  {offer.kind === "choice" ? (
                    <>
                      <span className="slash-name">{offer.choice.label}</span>
                      <span className="slash-arg mono">{offer.choice.value}</span>
                      <span className="slash-hint">{offer.choice.hint}</span>
                    </>
                  ) : (
                    <>
                      <span className="slash-name mono">/{offer.command.name}</span>
                      <span className="slash-label">{offer.command.label}</span>
                      {offer.command.arg && <span className="slash-arg">{offer.command.arg}</span>}
                      <span className="slash-hint">{offer.command.unavailable ?? offer.command.hint}</span>
                    </>
                  )}
                </div>
              ))}
              <p className="menu-note">↑↓ 选，Tab 补全，回车执行，Esc 收起</p>
            </div>
          )}
          <label htmlFor="composer-input" className="sr-only">
            给分身的法旨
          </label>
          <textarea
            id="composer-input"
            ref={input}
            rows={2}
            value={text}
            placeholder={placeholder ?? (busy ? "给分身传一句音" : "吩咐一件事，回车传令；打 / 唤出命令")}
            aria-controls={menu.length > 0 ? "slash-menu" : undefined}
            aria-activedescendant={menu.length > 0 ? `slash-${Math.min(chosen, menu.length - 1)}` : undefined}
            onChange={(event) => setText(event.target.value)}
            // The button's menu belongs to the box while the box has the keyboard: pressed elsewhere, it is put away.
            onBlur={() => setBrowsing(false)}
            onPaste={(event) => {
              // A pasted screenshot is attached; pasted text goes into the box as usual.
              const files = [...event.clipboardData.items].filter((item) => item.kind === "file" && item.type.startsWith("image/")).flatMap((item) => item.getAsFile() ?? []);
              if (files.length === 0) return;
              event.preventDefault();
              void attach(files);
            }}
            onKeyDown={(event) => {
              // While the "/" menu is up, the arrows go through it, Tab completes, Enter takes, Escape puts it away.
              if (menu.length > 0 && !event.nativeEvent.isComposing) {
                if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                  event.preventDefault();
                  setChosen((at) => (at + (event.key === "ArrowDown" ? 1 : menu.length - 1)) % menu.length);
                  return;
                }
                if (event.key === "Tab" && !event.shiftKey) {
                  event.preventDefault();
                  setBrowsing(false);
                  setText(completion(menu[Math.min(chosen, menu.length - 1)]));
                  return;
                }
                if (event.key === "Enter" && !event.shiftKey) {
                  event.preventDefault();
                  accept(menu[Math.min(chosen, menu.length - 1)]);
                  return;
                }
                if (event.key === "Escape") {
                  event.preventDefault();
                  setBrowsing(false);
                  setDismissed(text);
                  return;
                }
              }
              // Enter sends; Shift+Enter breaks the line. Enter that confirms an input-method candidate does neither.
              if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
                event.preventDefault();
                submit();
              }
              // Shift+Tab cycles the 禁制 instead of moving focus back.
              if (event.key === "Tab" && event.shiftKey) {
                event.preventDefault();
                onMode(nextMode(mode));
              }
            }}
          />
          <div className="composer-row">
            {/* First, where the hand goes to add something to the message; then how the 分身 stands. */}
            <button
              type="button"
              className="select-button attach"
              title="附文件或图给分身：文本文件和 PDF、Word、Excel、PPT 取出文字一起发，太大的由分身自己分段读；附上的文件，分身读它不用请示。也可以把文件拖进窗口，或把截图粘贴进输入框"
              onClick={() => api.pickFiles().then(take, (error) => setRefused(`没附上：${String(error)}`))}
            >
              {/* A paperclip. */}
              <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                <path d="M13 7.4 8 12.4a3 3 0 0 1-4.3-4.2l5.4-5.4a2 2 0 0 1 2.9 2.8L6.7 10.9a1 1 0 0 1-1.5-1.4l4.6-4.6" />
              </svg>
              <span className="sr-only">附件</span>
            </button>
            {/* The "/" commands, for a hand on the pointer: the same menu typing "/" brings up, with every command in it. */}
            <div className="mode-picker">
              <button
                type="button"
                className="select-button"
                aria-haspopup="listbox"
                aria-expanded={menu.length > 0}
                aria-controls={menu.length > 0 ? "slash-menu" : undefined}
                title="命令：也可以在输入框行首打 / 唤出"
                // The box keeps the keyboard, so the arrows, Enter and Escape work in the menu as when it is typed up.
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => {
                  input.current?.focus();
                  if (menu.length > 0) {
                    setBrowsing(false);
                    setDismissed(text);
                  } else {
                    setDismissed(null);
                    setChosen(0);
                    setBrowsing(true);
                  }
                }}
              >
                <span className="slash-glyph" aria-hidden="true">
                  /
                </span>
                <span className="label sr-only">命令</span>
              </button>
            </div>
            <Popup
              name="心法"
              summon={modelSummon}
              disabled={busy}
              title={busy ? `心法：${providerName} · ${model}。分身运功时不能换，等它收功` : `心法：${providerName} · ${model}。点开换一门`}
              label={
                <>
                  <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M3.5 2.5h8a1 1 0 0 1 1 1v10h-8a1 1 0 0 1-1-1zM3.5 11.5a1 1 0 0 1 1-1h8M6 5.5h4" />
                  </svg>
                  <span className="label sr-only">心法</span>
                  <span className="value">{model}</span>
                </>
              }
            >
              {(close) => (
                <ModelList
                  providers={providers}
                  listed={listed}
                  keyed={keyed}
                  providerId={providerId}
                  model={model}
                  listModels={listModels}
                  onPick={(pickedProvider, picked) => {
                    if (pickedProvider !== providerId || picked !== model) onModel(pickedProvider, picked);
                    close();
                  }}
                />
              )}
            </Popup>
            <Popup
              name="境界"
              title={`境界：${EFFORT_NAMES[effort] ?? effort}，${effortSet ? "这个分身单独定的" : "跟着设置里的默认"}。点开换一档`}
              label={
                <>
                  {/* Three steps, each higher than the last. */}
                  <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round">
                    <path d="M2.5 13.5h3v-3h-3zM6.5 13.5h3v-6h-3zM10.5 13.5h3v-9h-3z" />
                  </svg>
                  <span className="label sr-only">境界</span>
                  <span className="value">{EFFORT_NAMES[effort] ?? effort}</span>
                </>
              }
            >
              {(close) => (
                <>
                  <div className="mode-menu-head">
                    <span>境界</span>
                    <span>{effortSet ? "这个分身单独定的" : "跟着设置里的默认"}</span>
                  </div>
                  {efforts.map((level) => (
                    <button
                      type="button"
                      role="menuitemradio"
                      aria-checked={level === effort}
                      key={level}
                      className="mode-option plain"
                      onClick={() => {
                        // Choosing the default's own level still sets this 分身 apart: the settings may change later.
                        if (level !== effort || !effortSet) onEffort(level);
                        close();
                      }}
                    >
                      <span className="mode-text">
                        <span className="name">{EFFORT_NAMES[level] ?? level}</span>
                        <span className="hint">{EFFORT_HINTS[level]}</span>
                      </span>
                      {level === effort ? CHECK : <span />}
                    </button>
                  ))}
                  <p className="menu-note">只换这个分身，下一次请求起生效；新分身用设置里的默认。{effortNote(provider)}</p>
                </>
              )}
            </Popup>
            <ModePicker mode={mode} onMode={onMode} trustable={trustable} />
            <Popup
              name="灵石"
              title={`灵石：前情占了 ${shortCount(usage.context)}，到 ${shortCount(contextLimit)} 就凝成摘要。点开看累计用量、改上限`}
              label={
                <>
                  <Stones part={contextLimit > 0 ? usage.context / contextLimit : 0} />
                  <span className="label sr-only">灵石</span>
                  <span className="value">
                    {shortCount(usage.context)} / {shortCount(contextLimit)}
                  </span>
                </>
              }
            >
              {(close) => (
                <>
                  <div className="mode-menu-head">
                    <span>灵石</span>
                    <span>这个分身累计</span>
                  </div>
                  <p className="menu-note">
                    输入 {shortCount(usage.spent.input)}（其中 {shortCount(usage.spent.cached)} 读自缓存，便宜得多），输出 {shortCount(usage.spent.output)}。眼下的前情占 {shortCount(usage.context)}。
                  </p>
                  <button
                    type="button"
                    role="menuitem"
                    className="mode-option plain"
                    disabled={busy || usage.context === 0}
                    title={busy ? "分身运功时不能凝练，等它收功" : usage.context === 0 ? "还没有前情可凝" : undefined}
                    onClick={() => {
                      onCompact();
                      close();
                    }}
                  >
                    <span className="mode-text">
                      <span className="name">现在就凝成摘要</span>
                      <span className="hint">不等到上限：让分身把前情凝成一段摘要，之后的请求省灵石。摘要会丢掉细节</span>
                    </span>
                    <span />
                  </button>
                  <div className="mode-menu-head">
                    <span>前情上限</span>
                    <span>到了就凝成摘要</span>
                  </div>
                  <button
                    type="button"
                    role="menuitemradio"
                    aria-checked={contextAuto}
                    className="mode-option plain"
                    onClick={() => {
                      if (!contextAuto) onLimit("auto");
                      close();
                    }}
                  >
                    <span className="mode-text">
                      <span className="name">自动</span>
                      <span className="hint">按心法自己的窗口，用到八成：这门是 {shortCount(contextCeiling)}</span>
                    </span>
                    {contextAuto ? CHECK : <span />}
                  </button>
                  {limits.map((limit) => {
                    // A setting above what the model can hold lands on the model's own ceiling.
                    const chosen = !contextAuto && limit === Math.min(contextSetting, contextCeiling);
                    return (
                      <button
                        type="button"
                        role="menuitemradio"
                        aria-checked={chosen}
                        key={limit}
                        className="mode-option plain"
                        onClick={() => {
                          if (!chosen) onLimit(limit);
                          close();
                        }}
                      >
                        <span className="mode-text">
                          <span className="name">{shortCount(limit)}</span>
                          {limit === contextCeiling && <span className="hint">这门心法装得下的最多</span>}
                        </span>
                        {chosen ? CHECK : <span />}
                      </button>
                    );
                  })}
                  <p className="menu-note">每次请求都要把整段前情发给心法，上限越高，长对话越费灵石。对所有分身生效；别的数可以到设置里填。</p>
                </>
              )}
            </Popup>
            <span className="spacer" />
            {/* A 令牌, the tablet an order is sent with: 令 on jade for an order, 音 on gold for a word to a 分身 at work.
                While the 分身 works the token is away with the order, and shows only when there is a word to send after it. */}
            {(!busy || !empty) && (
              <button type="button" key={busy ? "word" : "order"} ref={sender} className="token-button" disabled={empty} title={busy ? "传音：分身做完这一步就会听到" : "传令（回车）"} onClick={submit}>
                <svg className={`token ${busy ? "word" : ""}`} aria-hidden="true" width="26" height="32" viewBox="0 0 32 40">
                  <path className="plate" d="M7 1.5h18l5.5 5.5v21L16 38.5 1.5 28V7Z" />
                  <path className="rim" d="M8.2 4.5h15.6l3.7 3.7v18.3L16 34.8 4.5 26.5V8.2Z" />
                  <circle className="hole" cx="16" cy="8.6" r="1.7" />
                  <text x="16" y="26" textAnchor="middle">
                    {busy ? "音" : "令"}
                  </text>
                </svg>
                <span className="sr-only">{busy ? "传音" : "传令"}</span>
              </button>
            )}
            {/* In the token's place while the 分身 works: a token of cinnabar, the order that calls it back. */}
            {busy && (
              <button type="button" className="token-button stop" title="收回分身：停下这一轮" onClick={onStop}>
                <svg className="token halt" aria-hidden="true" width="26" height="32" viewBox="0 0 32 40">
                  <path className="plate" d="M7 1.5h18l5.5 5.5v21L16 38.5 1.5 28V7Z" />
                  <path className="rim" d="M8.2 4.5h15.6l3.7 3.7v18.3L16 34.8 4.5 26.5V8.2Z" />
                  <circle className="hole" cx="16" cy="8.6" r="1.7" />
                  <text x="16" y="26" textAnchor="middle">
                    止
                  </text>
                </svg>
                <span className="sr-only">收回分身</span>
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
