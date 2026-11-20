import { useEffect, useRef, useState } from "react";
import type { Image } from "../lib/types";

/** A picture as a page can show it. */
export const imageSource = (image: Image) => `data:${image.media_type};base64,${image.data}`;

interface ViewerProps {
  images: Image[];
  /** Which to show first. */
  at: number;
  /** What the pictures are, for a reader: "本尊附的", "要附的". */
  whose: string;
  onClose: () => void;
}

/**
 * A picture seen at full size, over everything, on a dimmed page: as large as the window lets it be, never larger
 * than it is. With several, the arrows (on the screen and on the keyboard) go from one to the next. Pressing
 * outside the picture, Escape or the close button puts it away.
 */
export function Viewer({ images, at, whose, onClose }: ViewerProps) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [shown, setShown] = useState(Math.min(Math.max(at, 0), images.length - 1));
  useEffect(() => {
    dialog.current?.showModal();
    // The keyboard to the window itself, for the arrows and Escape: not to its first button, which would wear a ring.
    dialog.current?.focus();
  }, []);
  const many = images.length > 1;
  const go = (step: number) => setShown((now) => (now + step + images.length) % images.length);
  const image = images[shown];
  if (!image) return null;
  return (
    <dialog
      ref={dialog}
      className="viewer"
      tabIndex={-1}
      aria-label={`${whose}第 ${shown + 1} 张图`}
      onClose={onClose}
      // The dialog fills the window; a press on it, not on the picture or a button, is a press outside the picture.
      onClick={(event) => event.target === event.currentTarget && dialog.current?.close()}
      onKeyDown={(event) => {
        if (!many) return;
        if (event.key === "ArrowLeft") go(-1);
        if (event.key === "ArrowRight") go(1);
      }}
    >
      <img src={imageSource(image)} alt={`${whose}第 ${shown + 1} 张图`} />
      <button type="button" className="viewer-close" aria-label="关上" title="关上（Esc）" onClick={() => dialog.current?.close()}>
        <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round">
          <path d="M3.5 3.5l9 9M12.5 3.5l-9 9" />
        </svg>
      </button>
      {many && (
        <>
          <button type="button" className="viewer-step back" aria-label="上一张" title="上一张（←）" onClick={() => go(-1)}>
            <svg aria-hidden="true" width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
              <path d="m10 3.5-4.5 4.5 4.5 4.5" />
            </svg>
          </button>
          <button type="button" className="viewer-step next" aria-label="下一张" title="下一张（→）" onClick={() => go(1)}>
            <svg aria-hidden="true" width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
              <path d="m6 3.5 4.5 4.5L6 12.5" />
            </svg>
          </button>
          <span className="viewer-count" aria-live="polite">
            {shown + 1} / {images.length}
          </span>
        </>
      )}
    </dialog>
  );
}
