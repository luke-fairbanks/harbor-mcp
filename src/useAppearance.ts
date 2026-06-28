import { useEffect, useState } from "react";

/** Track the system light/dark appearance so Radix and our tokens stay in sync. */
export function useAppearance(): "light" | "dark" {
  const [appearance, setAppearance] = useState<"light" | "dark">(() =>
    window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light",
  );
  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (e: MediaQueryListEvent) =>
      setAppearance(e.matches ? "dark" : "light");
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);
  return appearance;
}
