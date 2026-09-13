import { Check, CircleDashed, LoaderCircle, X } from "lucide-react";
import type { JobStatus } from "@/api/client";
import { statusNames } from "@/lib/format";

/** Uses both a label and an icon so task state never depends on color alone. */
export function StatusBadge({ status }: { status: JobStatus }) {
  const Icon = {
    queued: CircleDashed,
    running: LoaderCircle,
    succeeded: Check,
    failed: X,
  }[status];
  return (
    <span className={`status-badge status-${status}`}>
      <Icon
        aria-hidden="true"
        className={status === "running" ? "animate-spin" : ""}
        size={13}
      />
      {statusNames[status]}
    </span>
  );
}
