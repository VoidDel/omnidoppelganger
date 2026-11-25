import { stepped } from "../lib/replay";
import { Icon } from "./Icon";

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
        回溯中：页面上是这个分身当时的样子，右边的改动、终端、计划也是。看的是记录，什么都不会改动。
      </div>
      <div className="composer-box replay" role="group" aria-label="回溯">
        <label htmlFor="replay-at" className="sr-only">
          回溯到第几步
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
            <Icon name="skip-start-fill" size={16} />
            <span className="sr-only">回到开头</span>
          </button>
          <button type="button" className="icon-button" title="上一步" disabled={at === 0} onClick={() => step(-1)}>
            <Icon name="chevron-left" size={15} />
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
            <Icon name={playing ? "pause-fill" : "play-fill"} size={16} />
            <span className="sr-only">{playing ? "暂停" : "播放"}</span>
          </button>
          <button type="button" className="icon-button" title="下一步" disabled={atEnd} onClick={() => step(1)}>
            <Icon name="chevron-right" size={15} />
            <span className="sr-only">下一步</span>
          </button>
          <span className="replay-where">
            第 {at + 1} / {count} 步<span className="muted">，第 {turn} 轮，{time}</span>
          </span>
          <span className="spacer" />
          <button type="button" className="icon-button replay-speed" title={`快慢：现在 ${speed} 倍，点一下换一档`} onClick={() => onSpeed(SPEEDS[(SPEEDS.indexOf(speed) + 1) % SPEEDS.length])}>
            <span aria-hidden="true">{speed}x</span>
            <span className="sr-only">快慢 {speed} 倍</span>
          </button>
          <button type="button" className="icon-button" title="退出回溯，回到现在" onClick={onExit}>
            <Icon name="stop-fill" size={15} />
            <span className="sr-only">退出回溯</span>
          </button>
        </div>
      </div>
    </div>
  );
}
