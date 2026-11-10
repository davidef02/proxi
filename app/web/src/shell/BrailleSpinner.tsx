import { useEffect, useState } from "react";

const FRAMES = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

/** The one working-state glyph (terminal braille, 80 ms/frame). */
export function BrailleSpinner({ label = "In corso", className = "" }: { label?: string; className?: string }) {
  const [i, setI] = useState(0);
  useEffect(() => {
    const t = setInterval(() => setI((n) => (n + 1) % FRAMES.length), 80);
    return () => clearInterval(t);
  }, []);
  return (
    <span role="status" aria-label={label} className={`inline-block w-[1ch] text-center ${className}`}>
      {FRAMES[i]}
    </span>
  );
}
