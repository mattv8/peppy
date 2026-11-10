import { copySegments } from "./groupCopy";
import { type GroupNoticeItem, groupNoticeCopy } from "./groupNotices";

type GroupNoticeProps = {
  notice: GroupNoticeItem;
  displayName: (address: string) => string;
};

/** A timeline separator for a membership or name change. Names are isolated with `<bdi>`. */
export function GroupNotice({ notice, displayName }: GroupNoticeProps) {
  const copy = groupNoticeCopy(notice, displayName);
  return <p id={notice.id} className="timeline-notice" data-group-notice={notice.kind} data-group-notice-revision={notice.revision} title={copy.title}>
    <span className="timeline-notice-text">
      {copySegments(copy.template, copy.values).map((segment) => segment.value
        ? <bdi key={segment.offset}>{segment.text}</bdi>
        : <span key={segment.offset}>{segment.text}</span>)}
    </span>
  </p>;
}
