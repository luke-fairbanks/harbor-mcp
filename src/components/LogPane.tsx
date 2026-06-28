import { useEffect, useMemo, useRef, useState } from "react";
import { SegmentedControl } from "@radix-ui/themes";
import type { LogLine } from "../types";

export function LogPane({
  logs,
  services,
}: {
  logs: LogLine[];
  services: string[];
}) {
  const [filter, setFilter] = useState<string>("all");
  const ref = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

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

  return (
    <div className="log-wrap">
      <div className="row">
        <span className="section-label">Logs</span>
        <span className="spacer" />
        {multi && (
          <SegmentedControl.Root
            size="1"
            value={filter}
            onValueChange={(v) => v && setFilter(v)}
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
      <div className="log-pane" ref={ref} onScroll={onScroll}>
        {shown.length === 0 ? (
          <div className="log-empty">No output yet.</div>
        ) : (
          shown.map((l) => (
            <div className="log-line" data-stream={l.stream} key={l.seq}>
              {filter === "all" && multi && <span className="svc">{l.service}</span>}
              <span>{l.line || " "}</span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
