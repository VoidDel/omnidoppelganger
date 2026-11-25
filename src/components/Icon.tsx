import type { CSSProperties } from "react";

export interface IconProps {
  /** Bootstrap Icons glyph name without the `bi-` prefix (e.g. `"box-seam"`, `"terminal"`). */
  name: string;
  /** Icon size in pixels (defaults to 15). */
  size?: number;
  className?: string;
  style?: CSSProperties;
}

/** Unified Bootstrap Icons wrapper (`bootstrap-icons` MIT). */
export function Icon({ name, size = 15, className, style }: IconProps) {
  return (
    <i
      aria-hidden="true"
      className={className ? `bi bi-${name} ${className}` : `bi bi-${name}`}
      style={{ fontSize: `${size}px`, ...style }}
    />
  );
}
