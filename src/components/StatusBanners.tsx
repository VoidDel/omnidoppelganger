import { useState } from "react";
import { useI18n } from "../i18n";
import { api } from "../lib/api";
import { localizeBackendError } from "../lib/errorCode";
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
  const { locale, t } = useI18n();
  const [dismissedTrust, setDismissedTrust] = useState<string[]>([]);
  const separator = locale === "en" ? ", " : "、";

  return (
    <>
      {error && (
        <div className="error-banner" role="alert">
          <span style={{ flex: 1, overflowWrap: "anywhere" }}>{localizeBackendError(error, locale)}</span>
          <button type="button" className="seal seal-button" title={t("banner.dismissTitle")} onClick={onDismissError}>
            {t("banner.dismissSeal")}
            <span className="sr-only">{t("banner.dismissTitle")}</span>
          </button>
        </div>
      )}
      {note && (
        <div className="error-banner note" role="status">
          <span style={{ flex: 1, overflowWrap: "anywhere" }}>{note}</span>
          <button type="button" className="seal seal-button" title={t("banner.dismissTitle")} onClick={onDismissNote}>
            {t("banner.dismissSeal")}
            <span className="sr-only">{t("banner.dismissTitle")}</span>
          </button>
        </div>
      )}
      {active &&
        !replaying &&
        active.project_trusted === false &&
        (active.untrusted_resources?.length ?? 0) > 0 &&
        !dismissedTrust.includes(active.workspace) && (
          <div className="error-banner note" role="region" aria-label={t("banner.trustRegionAria")}>
            <span style={{ flex: 1, overflowWrap: "anywhere" }}>
              {t("banner.trustPrompt", { resources: active.untrusted_resources!.join(separator) })}
            </span>
            <button
              type="button"
              className="button primary small"
              onClick={() => {
                api.setWorkspaceTrust(active.id, true).then(
                  (item) => onTrusted(item, t("banner.trustedNote")),
                  onError,
                );
              }}
            >
              {t("banner.trustAccept")}
            </button>
            <button
              type="button"
              className="button outline small"
              onClick={() => setDismissedTrust((prev) => [...prev, active.workspace])}
            >
              {t("banner.trustLater")}
            </button>
          </div>
        )}
    </>
  );
}
