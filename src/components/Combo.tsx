import { useEffect, useState } from "react";
import { Icon } from "./Icon";

interface ComboProps {
  id: string;
  /** The choice that is saved. */
  value: string;
  /** The choices: what each is saved as, the words it is shown by, and a note set beside them in the list. */
  options: { value: string; text: string; note?: string }[];
  /** Whether something that is not among the choices can be typed in: a model's id, say. */
  editable: boolean;
  /** Set the choices in the face used for code. */
  mono?: boolean;
  placeholder?: string;
  onSave: (value: string) => void;
}

/**
 * A field with a list under it, one look for every such choice on the page. The list opens with every choice in it,
 * whatever the field holds: the browser's own suggestions show only those matching what is already there, which
 * hides most of them. In a field that can be typed in, typing narrows the list.
 */
export function Combo({ id, value, options, editable, mono, placeholder, onSave }: ComboProps) {
  const shownFor = (kept: string) => (editable ? kept : (options.find((option) => option.value === kept)?.text ?? kept));
  const [draft, setDraft] = useState(shownFor(value));
  useEffect(() => setDraft(shownFor(value)), [value, options.length]);
  const [open, setOpen] = useState(false);
  /** Whether the user has typed since the list opened: only then does what is typed narrow it. */
  const [typed, setTyped] = useState(false);
  const [at, setAt] = useState(-1);
  const wanted = draft.trim().toLowerCase();
  const shown = typed && wanted ? options.filter((option) => option.value.toLowerCase().includes(wanted) || option.text.toLowerCase().includes(wanted)) : options;
  const close = () => {
    setOpen(false);
    setTyped(false);
    setAt(-1);
  };
  const commit = (picked: string) => {
    close();
    setDraft(shownFor(picked.trim()));
    if (picked.trim() !== value) onSave(picked.trim());
  };
  return (
    <div className="combo">
      <input
        id={id}
        className={`input ${mono ? "mono" : ""}`}
        role="combobox"
        aria-expanded={open}
        aria-controls={`${id}-list`}
        aria-autocomplete={editable ? "list" : "none"}
        autoComplete="off"
        readOnly={!editable}
        placeholder={placeholder}
        value={draft}
        onFocus={() => setOpen(true)}
        onClick={() => setOpen(true)}
        onChange={(event) => {
          setDraft(event.target.value);
          setTyped(true);
          setOpen(true);
          setAt(-1);
        }}
        // Leaving the field keeps what was typed; picking from the list does not leave it (see the options).
        onBlur={() => (editable ? commit(draft) : close())}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            setOpen(true);
            if (shown.length > 0) setAt((now) => (now + (event.key === "ArrowDown" ? 1 : -1) + shown.length) % shown.length);
          }
          if (event.key === "Enter") {
            if (open && at >= 0 && shown[at]) commit(shown[at].value);
            else if (editable) commit(draft);
          }
          if (event.key === "Escape" && open) {
            // The list is what Escape puts away: not the window the field may be in.
            event.preventDefault();
            event.stopPropagation();
            close();
            setDraft(shownFor(value));
          }
        }}
      />
      <Icon name="chevron-down" size={12} className="combo-mark" />
      {open && (
        <ul id={`${id}-list`} className="combo-list" role="listbox">
          {shown.map((option, index) => (
            <li
              key={option.value}
              role="option"
              aria-selected={option.value === value}
              className={index === at ? "at" : ""}
              // Before the field would lose focus, so the pick is not overtaken by what the field holds.
              onMouseDown={(event) => {
                event.preventDefault();
                commit(option.value);
              }}
            >
              <span className={mono ? "mono" : ""}>{option.text}</span>
              {option.note && <span className="muted">{option.note}</span>}
            </li>
          ))}
          {shown.length === 0 && <li className="none">没有对得上的。照这样填也行，回车保存。</li>}
        </ul>
      )}
    </div>
  );
}
