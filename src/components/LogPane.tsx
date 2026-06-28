import { useEffect, useMemo, useRef, useState } from "react";
import { Flex, SegmentedControl, Text } from "@radix-ui/themes";
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
  const stickRef = useRef(true);

  const shown = useMemo(
    () => (filter === "all" ? logs : logs.filter((l) => l.service === filter)),
    [logs, filter],
  );

  // Auto-scroll to bottom unless the user scrolled up.
  useEffect(() => {
    const el = ref.current;
    if (el && stickRef.current) el.scrollTop = el.scrollHeight;
  }, [shown]);

  const onScroll = () => {
    const el = ref.current;
    if (!el) return;
    stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  };

  return (
    <Flex direction="column" gap="2" className="fill">
      <Flex align="center" justify="between">
        <Text size="1" color="gray" weight="medium">
          LOGS
        </Text>
        {services.length > 1 && (
          <SegmentedControl.Root
            size="1"
            value={filter}
            onValueChange={setFilter}
          >
            <SegmentedControl.Item value="all">all</SegmentedControl.Item>
            {services.map((s) => (
              <SegmentedControl.Item key={s} value={s}>
                {s}
              </SegmentedControl.Item>
            ))}
          </SegmentedControl.Root>
        )}
      </Flex>
      <div className="log-pane" ref={ref} onScroll={onScroll}>
        {shown.length === 0 ? (
          <Text size="1" color="gray">
            No output yet.
          </Text>
        ) : (
          shown.map((l) => (
            <div className="log-line" data-stream={l.stream} key={l.seq}>
              {filter === "all" && services.length > 1 && (
                <span className="svc">{l.service}</span>
              )}
              <span>{l.line || " "}</span>
            </div>
          ))
        )}
      </div>
    </Flex>
  );
}
