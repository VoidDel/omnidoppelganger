import { stepped } from "../lib/replay";

/** How fast a replay can go: steps a second, roughly. */
export const SPEEDS = [1, 2, 4];
/** The time one step stays on the page at the slowest speed. */
export const STEP_MS = 900;

interface Props {
  /** The stop on the page, counted from 0, and how many there are. */
  at: number;
  count: number;
  playing: boolean;
  speed: number;
  /** The turn the stop is in, and the time of day it happened. */
  turn: number;
  time: string;
  onSeek: (at: number) => void;
  onPlaying: (playing: boolean) => void;
  onSpeed: (speed: number) => void;
  onExit: () => void;
}

const icon = (path: string, filled = false) => (
  <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill={filled ? "currentColor" : "none"} stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
    <path d={path} />
  </svg>
);

/**
 * The bar that stands where the message box does while a session is replayed:
 * the page shows the session as it stood at one step of its log, and this
 * moves through the steps. Nothing here changes the session.
 */
export function ReplayBar({ at, count, playing, speed, turn, time, onSeek, onPlaying, onSpeed, onExit }: Props) {
  const atEnd = at >= count - 1;
  const step = (by: number) => {
    onPlaying(false);
    onSeek(stepped(at, by, count));
  };
  return (
    <div className="composer">
      <div className="composer-status" role="status">
        回放中：页面上是这个分身当时的样子，右边的改动、终端、计划也是。看的是记录，什么都不会改动。
      </div>
      <div className="composer-box replay" role="group" aria-label="回放">
        <label htmlFor="replay-at" className="sr-only">
          回放到第几步
        </label>
        <input
          id="replay-at"
          className="replay-track"
          type="range"
          min={0}
          max={Math.max(0, count - 1)}
          step={1}
          value={at}
          aria-valuetext={`第 ${at + 1} 步，共 ${count} 步`}
          onChange={(event) => {
            onPlaying(false);
            onSeek(Number(event.target.value));
          }}
        />
        <div className="replay-row">
          <button type="button" className="icon-button" title="回到开头" disabled={at === 0} onClick={() => step(-count)}>
            {icon("M4 3.5v9M12.5 3.5 6.5 8l6 4.5z")}
            <span className="sr-only">回到开头</span>
          </button>
          <button type="button" className="icon-button" title="上一步" disabled={at === 0} onClick={() => step(-1)}>
            {icon("M10.5 3.5 5 8l5.5 4.5")}
            <span className="sr-only">上一步</span>
          </button>
          <button
            type="button"
            className="replay-play"
            title={playing ? "停在这一步" : atEnd ? "从头再放一遍" : "接着放"}
            onClick={() => {
              // At the end there is nothing left to play, so playing starts over.
              if (!playing && atEnd) onSeek(0);
              onPlaying(!playing);
            }}
          >
            {playing ? icon("M5.5 3.5v9M10.5 3.5v9") : icon("M5 3.2 12.5 8 5 12.8z", true)}
            <span className="sr-only">{playing ? "暂停" : "播放"}</span>
          </button>
          <button type="button" className="icon-button" title="下一步" disabled={atEnd} onClick={() => step(1)}>
            {icon("M5.5 3.5 11 8l-5.5 4.5")}
            <span className="sr-only">下一步</span>
          </button>
          <span className="replay-where">
            第 {at + 1} / {count} 步<span className="muted">，第 {turn} 轮，{time}</span>
          </span>
          <span className="spacer" />
          <button type="button" className="select-button" title="换一档快慢" onClick={() => onSpeed(SPEEDS[(SPEEDS.indexOf(speed) + 1) % SPEEDS.length])}>
            <span className="label">快慢</span>
            <span className="value">{speed} 倍</span>
          </button>
          <button type="button" className="button outline" onClick={onExit}>
            退出回放
          </button>
        </div>
      </div>
    </div>
  );
}
