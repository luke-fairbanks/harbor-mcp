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
