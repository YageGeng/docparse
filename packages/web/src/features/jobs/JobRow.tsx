import { useEffect, useId, useMemo, useRef, useState, type CSSProperties } from "react";
import { Link } from "react-router";
import { Popover } from "radix-ui";
import { AlertCircle, ArrowRight, Clock3, FileText, Info, LoaderCircle, Trash2 } from "lucide-react";
import type { Job } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { StatusBadge } from "@/components/StatusBadge";
import { fileSize, jobDuration, jobProgress, jobQueueDuration, jobTime } from "@/lib/format";

const nameSegments = new Intl.Segmenter(undefined, { granularity: "grapheme" });

/** Sizes the visible window from 24 real characters, including CJK and composed emoji. */
function DocumentName({ name, id, describedBy }: { name: string; id: string; describedBy?: string }) {
  const prefix = useMemo(() => Array.from(nameSegments.segment(name), ({ segment }) => segment).slice(0, 24).join(""), [name]);
  const viewport = useRef<HTMLSpanElement>(null);
  const text = useRef<HTMLSpanElement>(null);
  const [distance, setDistance] = useState(0);

  useEffect(() => {
    if (!viewport.current || !text.current) return;
    /** Measures actual overflow after responsive sizing and font changes, without guessing character widths. */
    function measure() {
      const overflow = (text.current?.scrollWidth ?? 0) - (viewport.current?.clientWidth ?? 0);
      setDistance(overflow > 1 ? overflow : 0);
    }
    const observer = new ResizeObserver(measure);
    observer.observe(viewport.current);
    observer.observe(text.current);
    measure();
    return () => observer.disconnect();
  }, [name]);

  return (
    <Link
      className="document-name"
      to={`/document?job=${id}`}
      target="_blank"
      rel="noopener noreferrer"
      aria-label={`${name}（在新标签页打开）`}
      aria-describedby={describedBy}
      data-overflow={distance > 0}
      style={{ "--name-distance": `${-distance}px`, "--name-duration": `${Math.max(5, distance / 28)}s` } as CSSProperties}
    >
      {/* Only this prefix contributes intrinsic width; the complete name never stretches the table. */}
      <span className="document-name-sizer" aria-hidden="true">{prefix}</span>
      <span className="document-name-viewport" ref={viewport} aria-hidden="true">
        <span className="document-name-text" ref={text}>{name}</span>
      </span>
    </Link>
  );
}

/** Keeps a document row compact while exposing the same live metadata to mouse, keyboard and touch users. */
export function JobRow({ job, now, open, onOpenChange, onDelete, deleting, deletingId }: {
  job: Job;
  now: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onDelete: (job: Job, trigger: HTMLButtonElement) => void;
  deleting: boolean;
  deletingId?: string;
}) {
  const row = useRef<HTMLTableRowElement>(null);
  const card = useRef<HTMLDivElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const detailsId = useId();
  const name = job.filename || `文档 ${job.id.slice(0, 8)}.pdf`;
  const progress = jobProgress(job);
  const pages = job.progress && "total" in job.progress ? job.progress.total : undefined;

  useEffect(() => () => clearTimeout(timer.current), []);

  /** Cancels delayed hover work when a click, Escape or outside interaction changes the preview. */
  function changePreview(next: boolean) {
    clearTimeout(timer.current);
    onOpenChange(next);
  }

  /** Bridges the pointer gap to the card and keeps a keyboard-focused row's description visible. */
  function schedulePreview(next: boolean) {
    clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      if (!next && (row.current?.contains(document.activeElement) || card.current?.contains(document.activeElement))) return;
      onOpenChange(next);
    }, next ? 240 : 180);
  }

  return (
    <Popover.Root open={open} onOpenChange={changePreview}>
      <Popover.Anchor asChild>
        <tr
          ref={row}
          data-preview={open}
          onPointerEnter={(event) => { if (event.pointerType !== "touch") schedulePreview(true); }}
          onPointerLeave={(event) => { if (event.pointerType !== "touch") schedulePreview(false); }}
          onFocusCapture={() => schedulePreview(true)}
          onBlurCapture={() => schedulePreview(false)}
        >
          <td>
            <div className="flex items-center gap-3">
              <button type="button" className="file-icon document-details-trigger" aria-label={`查看 ${name} 的详情`} aria-expanded={open} aria-controls={open ? detailsId : undefined} aria-haspopup="dialog" onClick={() => changePreview(!open)}>
                <FileText size={21} aria-hidden="true" /><Info className="document-details-indicator" size={14} aria-hidden="true" />
              </button>
              <div className="document-identity">
                <DocumentName name={name} id={job.id} describedBy={open ? `${detailsId}-summary` : undefined} />
                <p className="mt-1 text-xs text-muted-foreground">
                  {fileSize(job.size_bytes)}
                  <span className="mx-2 text-slate-300">·</span>
                  <span className="font-mono">{job.id.slice(0, 8)}</span>
                </p>
                <p className="mt-1 text-xs text-muted-foreground tabular-nums sm:hidden">排队耗时 {jobQueueDuration(job, now)}</p>
                <p className="mt-1 text-xs text-muted-foreground tabular-nums sm:hidden">解析耗时 {jobDuration(job)}</p>
                <p className="mt-1 text-xs text-muted-foreground tabular-nums sm:hidden">完成时间 {jobTime(job.finished_at)}</p>
              </div>
            </div>
          </td>
          <td><StatusBadge status={job.status} /></td>
          <td className="hidden whitespace-nowrap text-xs text-muted-foreground lg:table-cell">{progress.label}</td>
          <td className="hidden whitespace-nowrap text-xs text-muted-foreground tabular-nums sm:table-cell">{jobQueueDuration(job, now)}</td>
          <td className="hidden whitespace-nowrap text-xs text-muted-foreground tabular-nums sm:table-cell">{jobDuration(job)}</td>
          <td className="hidden whitespace-nowrap text-xs text-muted-foreground sm:table-cell">{jobTime(job.created_at)}</td>
          <td className="hidden whitespace-nowrap text-xs text-muted-foreground sm:table-cell">{jobTime(job.finished_at)}</td>
          <td>
            <div className="flex items-center justify-end gap-1">
              {/* Both links preserve the upload page and its active upload queue. */}
              <Link className="row-open" to={`/document?job=${job.id}`} target="_blank" rel="noopener noreferrer" aria-label={`在新标签页打开 ${name}`}><ArrowRight size={17} aria-hidden="true" /></Link>
              {(job.status === "succeeded" || job.status === "failed") && (
                <Button variant="ghost" size="icon-sm" className="text-muted-foreground hover:text-destructive" disabled={deleting} onClick={(event) => { changePreview(false); onDelete(job, event.currentTarget); }} aria-label={`删除 ${name} 的解析结果`}>
                  {deleting && deletingId === job.id ? <LoaderCircle className="animate-spin" size={16} /> : <Trash2 size={16} />}
                </Button>
              )}
            </div>
          </td>
        </tr>
      </Popover.Anchor>
      <Popover.Portal>
        <Popover.Content
          ref={card}
          id={detailsId}
          className="document-details"
          side="bottom"
          align="start"
          sideOffset={8}
          collisionPadding={16}
          hideWhenDetached
          aria-labelledby={`${detailsId}-title`}
          aria-describedby={`${detailsId}-summary`}
          onOpenAutoFocus={(event) => event.preventDefault()}
          onCloseAutoFocus={(event) => event.preventDefault()}
          onInteractOutside={(event) => {
            // Row controls belong to this preview, so focus changes and icon toggles cannot race outside dismissal.
            if (row.current?.contains(event.detail.originalEvent.target as Node)) event.preventDefault();
          }}
          onPointerEnter={() => schedulePreview(true)}
          onPointerLeave={() => schedulePreview(false)}
        >
          <div className="document-details-topline"><span>文档详情</span><StatusBadge status={job.status} /></div>
          <div className="document-details-heading">
            <div className="file-icon"><FileText size={22} aria-hidden="true" /></div>
            <div><h3 id={`${detailsId}-title`}>{name}</h3><p id={`${detailsId}-summary`}>PDF 文档<span>·</span>{pages == null ? "页数待确认" : `${pages} 页`}<span>·</span>{progress.label}</p></div>
          </div>
          {(job.status === "running" || job.status === "queued") && <div className={`document-details-progress is-${job.status}`}>
            <div><span>{progress.label}</span><span>{progress.percent == null ? "" : `${Math.round(progress.percent)}%`}</span></div>
            {progress.percent != null && <Progress value={progress.percent} aria-label={progress.label} />}
          </div>}
          <dl className="document-details-stats">
            <div><dt>文件大小</dt><dd>{fileSize(job.size_bytes)}</dd></div>
            <div><dt>处理次数</dt><dd>{job.attempts ? `${job.attempts} 次` : "尚未开始"}</dd></div>
            <div><dt>排队耗时</dt><dd>{jobQueueDuration(job, now)}</dd></div>
            <div><dt>解析耗时</dt><dd>{jobDuration(job)}</dd></div>
          </dl>
          <div className="document-details-timeline">
            <Clock3 size={14} aria-hidden="true" />
            <dl>
              <div><dt>创建时间</dt><dd>{jobTime(job.created_at)}</dd></div>
              <div><dt>开始解析</dt><dd>{jobTime(job.started_at)}</dd></div>
              <div><dt>完成时间</dt><dd>{jobTime(job.finished_at)}</dd></div>
            </dl>
          </div>
          {job.error && <div className="document-details-error"><AlertCircle size={15} aria-hidden="true" /><div><strong>解析异常</strong><p>{job.error}</p></div></div>}
          <div className="document-details-id"><span>任务 ID</span><code>{job.id}</code></div>
          <p className="document-details-hint">点击文档名称，在新标签页查看<ArrowRight size={13} aria-hidden="true" /></p>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
