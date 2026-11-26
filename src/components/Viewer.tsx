import { useCallback, useEffect, useRef, useState } from "react";
import type { Image } from "../lib/types";
import { Icon } from "./Icon";

/** A picture as a page can show it. */
export const imageSource = (image: Image) => `data:${image.media_type};base64,${image.data}`;

/** Decode a base64 Image into a PNG Blob suitable for `ClipboardItem` across WebKit/Chromium. */
async function imageToPngBlob(image: Image): Promise<Blob> {
  const src = imageSource(image);
  const res = await fetch(src);
  const raw = await res.blob();
  if (raw.type === "image/png") return raw;
  return new Promise<Blob>((resolve, reject) => {
    const img = new window.Image();
    img.onload = () => {
      const canvas = document.createElement("canvas");
      canvas.width = img.naturalWidth || img.width;
      canvas.height = img.naturalHeight || img.height;
      const ctx = canvas.getContext("2d");
      if (!ctx) {
        reject(new Error("Canvas unavailable"));
        return;
      }
      ctx.drawImage(img, 0, 0);
      canvas.toBlob((blob) => (blob ? resolve(blob) : reject(new Error("PNG encode failed"))), "image/png");
    };
    img.onerror = () => reject(new Error("Image decode failed"));
    img.src = src;
  });
}

/** Copy an image to the system clipboard (PNG ClipboardItem, with data-URL text fallback). */
export async function copyImageToClipboard(image: Image): Promise<boolean> {
  try {
    if (typeof ClipboardItem !== "undefined" && navigator.clipboard?.write) {
      const png = await imageToPngBlob(image);
      await navigator.clipboard.write([new ClipboardItem({ "image/png": png })]);
      return true;
    }
  } catch {
    // Fall through to text fallback if binary clipboard write is unavailable.
  }
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(imageSource(image));
      return true;
    }
  } catch {
    // ignore
  }
  return false;
}

const MIN_SCALE = 0.25;
const MAX_SCALE = 5;
const clampScale = (value: number) => Math.min(MAX_SCALE, Math.max(MIN_SCALE, Math.round(value * 100) / 100));

interface ViewerProps {
  images: Image[];
  /** Which to show first. */
  at: number;
  /** What the pictures are, for a reader: "本尊附的", "要附的". */
  whose: string;
  onClose: () => void;
  onAttachMain?: (image: Image) => void;
  onAttachAside?: (image: Image) => void;
}

/**
 * A picture seen at full size, over everything, on a dimmed page, with zoom/pan controls,
 * copy-to-clipboard, and quick attach to main/aside chat.
 */
export function Viewer({ images, at, whose, onClose, onAttachMain, onAttachAside }: ViewerProps) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [shown, setShown] = useState(Math.min(Math.max(at, 0), images.length - 1));
  const [scale, setScale] = useState(1);
  const [offset, setOffset] = useState({ x: 0, y: 0 });
  const [dragging, setDragging] = useState(false);
  const [copied, setCopied] = useState(false);
  const dragOrigin = useRef<{ x: number; y: number; ox: number; oy: number } | null>(null);

  const resetView = useCallback(() => {
    setScale(1);
    setOffset({ x: 0, y: 0 });
  }, []);

  const zoomBy = useCallback((factor: number) => {
    setScale((prev) => {
      const next = clampScale(prev * factor);
      if (next <= 1) setOffset({ x: 0, y: 0 });
      return next;
    });
  }, []);

  useEffect(() => {
    dialog.current?.showModal();
    dialog.current?.focus();
  }, []);

  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1500);
    return () => window.clearTimeout(timer);
  }, [copied]);

  useEffect(() => {
    const el = dialog.current;
    if (!el) return;
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      zoomBy(event.deltaY < 0 ? 1.15 : 1 / 1.15);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [zoomBy]);

  const many = images.length > 1;
  const go = (step: number) => {
    resetView();
    setShown((now) => (now + step + images.length) % images.length);
  };
  const image = images[shown];
  if (!image) return null;

  const pct = Math.round(scale * 100);

  return (
    <dialog
      ref={dialog}
      className="viewer"
      tabIndex={-1}
      aria-label={`${whose}第 ${shown + 1} 张图`}
      onClose={onClose}
      onClick={(event) => event.target === event.currentTarget && dialog.current?.close()}
      onKeyDown={(event) => {
        if (many && event.key === "ArrowLeft") {
          event.preventDefault();
          go(-1);
        } else if (many && event.key === "ArrowRight") {
          event.preventDefault();
          go(1);
        } else if (event.key === "+" || event.key === "=") {
          event.preventDefault();
          zoomBy(1.25);
        } else if (event.key === "-") {
          event.preventDefault();
          zoomBy(1 / 1.25);
        } else if (event.key === "0") {
          event.preventDefault();
          resetView();
        }
      }}
    >
      <img
        src={imageSource(image)}
        alt={`${whose}第 ${shown + 1} 张图`}
        draggable={false}
        className={scale > 1 ? (dragging ? "is-panning" : "can-pan") : "can-zoom"}
        style={{
          transform: `translate3d(${offset.x}px, ${offset.y}px, 0) scale(${scale})`,
        }}
        onDoubleClick={() => {
          if (scale !== 1) resetView();
          else setScale(2);
        }}
        onPointerDown={(event) => {
          if (scale <= 1 || event.button !== 0) return;
          event.currentTarget.setPointerCapture(event.pointerId);
          dragOrigin.current = { x: event.clientX, y: event.clientY, ox: offset.x, oy: offset.y };
          setDragging(true);
        }}
        onPointerMove={(event) => {
          if (!dragOrigin.current) return;
          setOffset({
            x: dragOrigin.current.ox + (event.clientX - dragOrigin.current.x),
            y: dragOrigin.current.oy + (event.clientY - dragOrigin.current.y),
          });
        }}
        onPointerUp={() => {
          dragOrigin.current = null;
          setDragging(false);
        }}
        onPointerCancel={() => {
          dragOrigin.current = null;
          setDragging(false);
        }}
      />
      <button type="button" className="viewer-close" aria-label="关上" title="关上（Esc）" onClick={() => dialog.current?.close()}>
        <Icon name="x-lg" size={16} />
      </button>
      {many && (
        <>
          <button type="button" className="viewer-step back" aria-label="上一张" title="上一张（←）" onClick={() => go(-1)}>
            <Icon name="chevron-left" size={18} />
          </button>
          <button type="button" className="viewer-step next" aria-label="下一张" title="下一张（→）" onClick={() => go(1)}>
            <Icon name="chevron-right" size={18} />
          </button>
        </>
      )}
      <div className="img-viewer-bar" role="toolbar" aria-label="图片缩放与操作">
        <button type="button" className="img-viewer-btn" aria-label="缩小" title="缩小（-）" disabled={scale <= MIN_SCALE} onClick={() => zoomBy(1 / 1.25)}>
          −
        </button>
        <button type="button" className="img-viewer-btn img-viewer-pct" title="重置为 100%（0）" onClick={resetView}>
          {pct}%
        </button>
        <button type="button" className="img-viewer-btn" aria-label="放大" title="放大（+）" disabled={scale >= MAX_SCALE} onClick={() => zoomBy(1.25)}>
          +
        </button>
        {many && (
          <span className="img-viewer-sep" aria-live="polite">
            {shown + 1} / {images.length}
          </span>
        )}
        <button
          type="button"
          className="img-viewer-btn"
          title="复制图片"
          onClick={() => void copyImageToClipboard(image).then((ok) => ok && setCopied(true))}
        >
          <Icon name={copied ? "check-lg" : "copy"} size={13} />
          <span>{copied ? "已复制" : "复制图片"}</span>
        </button>
        {onAttachMain && (
          <button
            type="button"
            className="img-viewer-btn"
            title="附到主对话输入框"
            onClick={() => {
              onAttachMain(image);
              dialog.current?.close();
            }}
          >
            <Icon name="quote" size={13} />
            <span>引用到对话</span>
          </button>
        )}
        {onAttachAside && (
          <button
            type="button"
            className="img-viewer-btn"
            title="附到右侧旁聊输入框"
            onClick={() => {
              onAttachAside(image);
              dialog.current?.close();
            }}
          >
            <Icon name="chat-left-quote" size={13} />
            <span>引用到旁聊</span>
          </button>
        )}
      </div>
    </dialog>
  );
}
