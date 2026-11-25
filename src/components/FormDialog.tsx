import { useEffect, useRef, type ClipboardEvent, type ReactNode } from "react";
import { Icon } from "./Icon";

interface FormDialogProps<Mode extends string> {
  /** The character on the seal at the head. */
  seal: string;
  title: string;
  /** The ways the form can be filled, as a segmented switch at the head's right; none for a form with one way. */
  modes?: [Mode, string][];
  mode?: Mode;
  onMode?: (mode: Mode) => void;
  /** A word beside the switch, such as that JSON can be pasted straight in. */
  aside?: ReactNode;
  /** What went wrong, under the fields; given what closes the window, for a way on from there that saves. */
  wrong?: ReactNode | ((close: () => void) => ReactNode);
  /** The buttons at the foot, the first of them the submit; given what closes the window. */
  actions: (close: () => void) => ReactNode;
  /** Given what closes the window, for once what was filled in is taken. */
  onSubmit: (close: () => void) => void;
  onPaste?: (event: ClipboardEvent<HTMLFormElement>) => void;
  onClose: () => void;
  children: ReactNode;
}

/**
 * A window for adding something by filling in a form: a head with its seal, title and the ways it can be filled,
 * rows of a label beside its field, and a foot with the buttons. Modal: the keyboard stays inside and Escape closes it.
 */
export function FormDialog<Mode extends string>({ seal, title, modes, mode, onMode, aside, wrong, actions, onSubmit, onPaste, onClose, children }: FormDialogProps<Mode>) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    dialog.current?.showModal();
    // Opened, the window gives the keyboard to the first button at its head; it belongs in the first field.
    dialog.current?.querySelector<HTMLElement>(".form-dialog-body input, .form-dialog-body textarea")?.focus();
  }, []);
  // Closed by the element itself, so Escape, the buttons and a save all end the same way, in `onClose`.
  const close = () => dialog.current?.close();
  return (
    <dialog ref={dialog} className="dialog form-dialog" aria-label={title} onClose={onClose}>
      <form
        method="dialog"
        onPaste={onPaste}
        onSubmit={(event) => {
          event.preventDefault();
          onSubmit(close);
        }}
      >
        <div className="form-dialog-head">
          <span className="seal" aria-hidden="true">
            {seal}
          </span>
          <h2>{title}</h2>
          {aside && <span className="form-dialog-aside">{aside}</span>}
          {modes && mode && onMode && <Segmented label="填写方式" options={modes} value={mode} onChange={onMode} />}
        </div>
        <div className="form-dialog-body">{children}</div>
        {wrong && (
          <div className="form-dialog-wrong hint failed" role="alert">
            {typeof wrong === "function" ? wrong(close) : wrong}
          </div>
        )}
        <div className="form-dialog-foot">{actions(close)}</div>
      </form>
    </dialog>
  );
}

/** A choice of a few, side by side, the one chosen raised. */
export function Segmented<Value extends string>({
  label,
  options,
  value,
  onChange,
}: {
  label: string;
  options: [Value, string][];
  value: Value;
  onChange: (value: Value) => void;
}) {
  return (
    <div className="segmented" role="group" aria-label={label}>
      {options.map(([option, text]) => (
        <button key={option} type="button" aria-pressed={option === value} onClick={() => onChange(option)}>
          {text}
        </button>
      ))}
    </div>
  );
}

/** One row of a form: its label at the left, the field and what explains it at the right. */
export function Row({ label, htmlFor, hint, children }: { label: string; htmlFor?: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <>
      <label className="form-row-label" htmlFor={htmlFor}>
        {label}
      </label>
      <div className="form-row-field">
        {children}
        {hint && <p className="hint">{hint}</p>}
      </div>
    </>
  );
}

/** Names with their values, such as environment variables or headers, one pair to a line, with a way to add another. */
export function Pairs({
  rows,
  onChange,
  keyHolder,
  valueHolder,
  what,
}: {
  rows: [string, string][];
  onChange: (rows: [string, string][]) => void;
  keyHolder: string;
  valueHolder: string;
  what: string;
}) {
  const set = (index: number, at: 0 | 1, text: string) => onChange(rows.map((row, i) => (i === index ? ((at === 0 ? [text, row[1]] : [row[0], text]) as [string, string]) : row)));
  return (
    <div className="pairs">
      {rows.map(([key, value], index) => (
        <div className="pair" key={index}>
          <input className="input mono" aria-label={`${what}名`} placeholder={keyHolder} value={key} onChange={(event) => set(index, 0, event.target.value)} />
          <input className="input mono" aria-label={`${what}值`} placeholder={valueHolder} value={value} onChange={(event) => set(index, 1, event.target.value)} />
          <button type="button" className="icon-button" title={`去掉这个${what}`} onClick={() => onChange(rows.filter((_, i) => i !== index))}>
            <Icon name="x-lg" size={12} />
            <span className="sr-only">去掉</span>
          </button>
        </div>
      ))}
      <button type="button" className="button text small pairs-add" onClick={() => onChange([...rows, ["", ""]])}>
        ＋ 添加
      </button>
    </div>
  );
}
