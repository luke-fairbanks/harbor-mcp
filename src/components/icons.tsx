/** Small line-art marks drawn to match SF Symbols weight. */

export function AnchorMark({ size = 17 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <circle cx="12" cy="5" r="2.3" />
      <line x1="12" y1="7.3" x2="12" y2="21" />
      <line x1="8.2" y1="10.6" x2="15.8" y2="10.6" />
      <path d="M4.8 13.6a7.2 7.2 0 0 0 14.4 0" />
    </svg>
  );
}

export function ProjectGlyph({
  name,
  compact = false,
}: {
  name: string;
  compact?: boolean;
}) {
  const initials =
    name
      .split(/[\s_-]+/)
      .filter(Boolean)
      .slice(0, 2)
      .map((part) => part[0]?.toUpperCase())
      .join("") || "P";
  const variant =
    [...name].reduce((sum, char) => sum + char.charCodeAt(0), 0) % 5;

  return (
    <span
      className="project-glyph"
      data-variant={variant}
      data-compact={compact || undefined}
      aria-hidden
    >
      {initials}
    </span>
  );
}

export function HarborBeacon({ size = 96 }: { size?: number }) {
  return (
    <span
      className="harbor-beacon"
      style={{ width: size, height: size }}
      aria-hidden
    >
      <span className="harbor-beacon-ring" />
      <span className="harbor-beacon-ring" />
      <span className="harbor-beacon-core">
        <AnchorMark size={Math.round(size * 0.28)} />
      </span>
    </span>
  );
}
