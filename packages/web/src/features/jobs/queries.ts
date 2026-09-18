import { useEffect, useState } from "react";
import {
  onlineManager,
  useInfiniteQuery,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import {
  apiUrl,
  ApiError,
  request,
  type Job,
  type JobList,
  type JobStatus,
} from "@/api/client";

/** Identifies states that must stop SSE subscriptions and polling. */
export const terminal = (job?: Job | null) =>
  job === null || job?.status === "succeeded" || job?.status === "failed";

/** Retrieves history in 50-item pages to reduce load-more requests; refresh runs only while mounted. */
export function useJobs(status: JobStatus | undefined, search: string) {
  return useInfiniteQuery({
    queryKey: ["jobs", status, search],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam, signal }) =>
      request<JobList>(
        "jobs/list",
        { cursor: pageParam, limit: 50, status, search: search || undefined },
        signal,
      ),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    refetchInterval: 5000,
  });
}

/** Keeps one active job live while preventing delayed polling responses from overwriting newer SSE snapshots. */
export function useJob(id: string) {
  const client = useQueryClient();
  // Browsers can leave a streaming socket open while offline; network state must override that stale connection state.
  const [online, setOnline] = useState(() => onlineManager.isOnline());
  useEffect(() => onlineManager.subscribe(setOnline), []);
  const [connection, setConnection] = useState<
    "connecting" | "live" | "reconnecting"
  >("connecting");
  const query = useQuery({
    queryKey: ["job", id],
    enabled: Boolean(id),
    queryFn: async ({ signal }) => {
      let incoming: Job;
      try {
        incoming = await request<Job>("jobs/status", { id }, signal);
      } catch (error) {
        // A missing task is a durable cache tombstone, not a refetch error that should retain old successful data.
        if (
          error instanceof ApiError &&
          error.status === 404 &&
          error.code === 4041001
        )
          return null;
        throw error;
      }
      const current = client.getQueryData<Job | null>(["job", id]);
      if (current === null) return null;
      return current && current.version > incoming.version ? current : incoming;
    },
    refetchInterval: (query) =>
      !terminal(query.state.data) && connection === "reconnecting"
        ? 5000
        : false,
  });
  const status = query.data?.status;
  useEffect(() => {
    if (!id || !status || status === "succeeded" || status === "failed") return;
    setConnection("connecting");
    const events = new EventSource(apiUrl("jobs/events", { id }));
    events.onopen = () => setConnection("live");
    events.addEventListener("job", (event: MessageEvent<string>) => {
      try {
        const incoming = JSON.parse(event.data).data as Job;
        if (incoming.id !== id || !Number.isSafeInteger(incoming.version))
          return;
        // Delayed SSE snapshots must not resurrect a task that a status read already confirmed missing.
        client.setQueryData<Job | null>(["job", id], (current) =>
          current === null
            ? null
            : !current || incoming.version > current.version
              ? incoming
              : current,
        );
        setConnection("live");
        if (terminal(incoming)) {
          events.close();
          void client.invalidateQueries({ queryKey: ["jobs"] });
        }
      } catch {
        // A malformed stream falls back to status polling instead of freezing the visible job.
        events.close();
        setConnection("reconnecting");
      }
    });
    events.onerror = () => setConnection("reconnecting");
    return () => events.close();
  }, [client, id, status]);
  return {
    ...query,
    notFound: query.data === null,
    connection: online ? connection : "reconnecting",
  };
}
