// Braille working-state glyph (idea from LearnHarness's braille-spinner; re-authored). CSS-only animation, see session.css.
const FRAMES: [number, number][][] = [
  [[1, 1], [1, 3], [3, 1]],
  [[1, 1], [3, 1], [3, 3]],
  [[1, 1], [3, 1], [3, 3], [3, 5]],
  [[3, 1], [3, 3], [3, 5]],
  [[1, 5], [3, 1], [3, 3], [3, 5]],
  [[1, 5], [3, 3], [3, 5]],
  [[1, 3], [1, 5], [3, 5]],
  [[1, 1], [1, 3], [1, 5], [3, 5]],
  [[1, 1], [1, 3], [1, 5]],
  [[1, 1], [1, 3], [1, 5], [3, 1]],
];

export function Spinner({ className = "h-3.5", label = "In corso" }: { className?: string; label?: string }) {
  return (
    <svg viewBox="0 0 4 8" role="status" aria-label={label} className={`${className} w-auto shrink-0 text-foreground`}>
      {FRAMES.map((dots, k) => (
        <g key={k} className="braille-frame" data-frame={k} style={{ animationDelay: `${k * 80}ms` }}>
          {dots.map(([cx, cy]) => (
            <circle key={`${cx}${cy}`} cx={cx} cy={cy} r={0.8} className="fill-current" />
          ))}
        </g>
      ))}
    </svg>
  );
}
