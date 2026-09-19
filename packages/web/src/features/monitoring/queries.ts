import { useRef } from "react";
import { useQuery } from "@tanstack/react-query";
import { request } from "@/api/client";
import {
  projectSnapshot,
  projectHistory,
  type Chart,
  type Matrix,
  type Snapshot,
} from "./metrics";

/** Polls one local recorder and resets counter differences when the responding process changes. */
export function useMonitoringSnapshot() {
  const previous = useRef<Snapshot | undefined>(undefined);
  return useQuery({
    queryKey: ["monitoring", "live"],
    queryFn: async ({ signal }) => {
      const current = await request<Snapshot>(
        "monitoring/snapshot",
        {},
        signal,
      );
      const view = projectSnapshot(current, previous.current);
      previous.current = current;
      return view;
    },
    refetchInterval: 5000,
    retry: false,
  });
}

/** Fetches one bounded historical chart while retaining prior data during upstream failures. */
export function useMonitoringHistory(
  chart: Chart,
  seconds: number,
  enabled: boolean,
) {
  return useQuery({
    queryKey: ["monitoring", "history", chart, seconds],
    queryFn: ({ signal }) =>
      request<Matrix>("monitoring/history", { chart, seconds }, signal),
    // Project a stable display order while preserving the raw query cache.
    select: projectHistory,
    enabled,
    refetchInterval: 30000,
    retry: false,
  });
}
