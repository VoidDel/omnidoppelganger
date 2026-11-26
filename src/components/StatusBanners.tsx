import { useState } from "react";
import { api } from "../lib/api";
import type { SessionItem } from "../lib/types";

export interface StatusBannersProps {
  error: string | null;
  note: string | null;
  active: SessionItem | undefined;
  replaying: boolean;
  onDismissError: () => void;
  onDismissNote: () => void;
  onTrusted: (item: SessionItem, note: string) => void;
  onError: (error: unknown) => void;
}

export function StatusBanners({
  error,
  note,
  active,
  replaying,
  onDismissError,
  onDismissNote,
  onTrusted,
  onError,
}: StatusBannersProps) {
  const [dismissedTrust, setDismissedTrust] = useState<string[]>([]);

  return (
    <>
      {error && (
        <div className="error-banner" role="alert">
          <span style={{ flex: 1, overflowWrap: "anywhere" }}>{error}</span>
          <button type="button" className="seal seal-button" title="阅过了，收起这一条" onClick={onDismissError}>
            阅<span className="sr-only">过了，收起这一条</span>
          </button>
        </div>
      )}
      {note && (
        <div className="error-banner note" role="status">
          <span style={{ flex: 1, overflowWrap: "anywhere" }}>{note}</span>
          <button type="button" className="seal seal-button" title="阅过了，收起这一条" onClick={onDismissNote}>
            阅<span className="sr-only">过了，收起这一条</span>
          </button>
        </div>
      )}
      {active &&
        !replaying &&
        active.project_trusted === false &&
        (active.untrusted_resources?.length ?? 0) > 0 &&
        !dismissedTrust.includes(active.workspace) && (
          <div className="error-banner note" role="region" aria-label="洞府项目信任确认">
            <span style={{ flex: 1, overflowWrap: "anywhere" }}>
              检测到此洞府含项目配置（{active.untrusted_resources!.join("、")}）。为防陌生仓库注入，未授信前不会自动加载。
            </span>
            <button
              type="button"
              className="button primary small"
              onClick={() => {
                api.setWorkspaceTrust(active.id, true).then(
                  (item) => onTrusted(item, "已信任此洞府，项目规矩与秘籍将在下一轮起生效。"),
                  onError,
                );
              }}
            >
              信任此洞府
            </button>
            <button
              type="button"
              className="button outline small"
              onClick={() => setDismissedTrust((prev) => [...prev, active.workspace])}
            >
              暂不加载
            </button>
          </div>
        )}
    </>
  );
}
