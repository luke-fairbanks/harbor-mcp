import { useEffect, useId, useMemo, useRef, useState } from "react";
import { SegmentedControl } from "@radix-ui/themes";
import type { LogLine } from "../types";

export function LogPane({
  logs,
  services,
  running,
}: {
  logs: LogLine[];
  services: string[];
  running?: boolean;
}) {
  const [filter, setFilter] = useState<string>("all");
  const ref = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const headingId = useId();

  const shown = useMemo(
    () => (filter === "all" ? logs : logs.filter((l) => l.service === filter)),
    [logs, filter],
  );

  useEffect(() => {
    const el = ref.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [shown]);

  const onScroll = () => {
    const el = ref.current;
    if (!el) return;
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
  };

  const multi = services.length > 1;
  const emptyCopy =
    filter !== "all"
      ? `No log output from ${filter} yet.`
      : running
        ? "Waiting for output. New log lines will appear here."
        : "No log output has been captured yet.";

  return (
    <section className="log-wrap" aria-labelledby={headingId}>
      <div className="row">
        <span className="section-label" id={headingId}>
          Logs
        </span>
        {running !== undefined && (
          <span
            className="chip"
            data-tone={running ? "ok" : undefined}
            role="status"
            aria-live="polite"
            aria-atomic="true"
          >
            {running ? "Live" : "Idle"}
          </span>
        )}
        <span className="spacer" />
        {multi && (
          <SegmentedControl.Root
            size="1"
            value={filter}
            onValueChange={(v) => v && setFilter(v)}
            aria-label="Filter log output by service"
          >
            <SegmentedControl.Item value="all">All</SegmentedControl.Item>
            {services.map((s) => (
              <SegmentedControl.Item key={s} value={s}>
                {s}
              </SegmentedControl.Item>
            ))}
          </SegmentedControl.Root>
        )}
      </div>
      <div
        className="log-pane"
        ref={ref}
        onScroll={onScroll}
        role="log"
        aria-label="Application log output"
        aria-live="off"
        tabIndex={0}
      >
        {shown.length === 0 ? (
          <div className="log-empty">{emptyCopy}</div>
        ) : (
          shown.map((l) => (
            <div className="log-line" data-stream={l.stream} key={l.seq}>
              {filter === "all" && multi && (
                <span className="svc">{l.service}</span>
              )}
              <span>{l.line || " "}</span>
            </div>
          ))
        )}
      </div>
    </section>
  );
}
