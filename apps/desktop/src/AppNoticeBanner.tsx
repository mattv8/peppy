export type AppNotice = {
  source: "refresh" | "action";
  severity: "error" | "warning" | "info";
  message: string;
  persistent?: boolean;
};

type AppNoticeBannerProps = {
  notices: AppNotice[];
  onDismiss?: (source: AppNotice["source"]) => void;
  onRetry?: () => void;
  retrying?: boolean;
};

const sourceOrder: AppNotice["source"][] = ["refresh", "action"];

function noticesBySource(notices: AppNotice[]) {
  const firstNoticeBySource = new Map<AppNotice["source"], AppNotice>();
  notices.forEach((notice) => {
    if (!firstNoticeBySource.has(notice.source)) firstNoticeBySource.set(notice.source, notice);
  });
  return sourceOrder.flatMap(source => {
    const notice = firstNoticeBySource.get(source);
    return notice ? [notice] : [];
  });
}

export function AppNoticeBanner({ notices, onDismiss, onRetry, retrying = false }: AppNoticeBannerProps) {
  const orderedNotices = noticesBySource(notices);
  if (!orderedNotices.length) return null;

  return <section id="app-notice-banner" aria-label="Application notices">
    {orderedNotices.map(notice => {
      const isError = notice.severity === "error";
      const canRetry = notice.source === "refresh" && onRetry !== undefined;
      const canDismiss = !notice.persistent && onDismiss;
      return <div
        key={notice.source}
        id={`app-notice-${notice.source}`}
        data-testid={`app-notice-${notice.source}`}
        data-app-notice-source={notice.source}
        data-app-notice-severity={notice.severity}
        className="app-notice-row"
      >
        <p
          key={`${notice.source}-${notice.severity}`}
          data-app-notice-message-source={notice.source}
          role={isError ? "alert" : "status"}
        >
          {notice.message}
        </p>
        <div className="app-notice-actions">
          {canRetry && <button
            type="button"
            className="secondary-button"
            aria-busy={retrying || undefined}
            disabled={retrying}
            onClick={() => onRetry()}
          >
            {retrying ? "Retrying refresh" : "Retry refresh"}
          </button>}
          {canDismiss && <button
            type="button"
            className="app-notice-dismiss"
            aria-label={`Dismiss ${notice.source} notice`}
            onClick={() => onDismiss(notice.source)}
          >
            Dismiss
          </button>}
        </div>
      </div>;
    })}
  </section>;
}
