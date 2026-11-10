import { useEffect, useId, useState } from "react";
import { mermaidVars } from "../theme-colors.ts";
import { token, useAppearance, type Appearance } from "../theme.ts";

const config = (a: Appearance) => ({
  startOnLoad: false,
  securityLevel: "strict" as const,
  theme: "base" as const,
  fontFamily: "JetBrains Mono, ui-monospace, monospace",
  themeVariables: mermaidVars(token, a.mode),
});
const cache = new Map<string, ReturnType<typeof config>>();

/** Mermaid needs literal colours (it derives shades from them): read once per palette × mode from the tokens in
 *  styles.css (theme-colors.ts maps them). The object is built once per combo, so Streamdown sees a stable config
 *  while a message streams. */
export const mermaidConfig = (a: Appearance) => {
  let c = cache.get(a.key);
  if (!c) {
    c = config(a);
    cache.set(a.key, c);
  }
  return c;
};

/** Renders a mermaid diagram; falls back to the source if it does not parse. */
export function Mermaid({ source, className = "" }: { source: string; className?: string }) {
  const id = "mm" + useId().replace(/[^a-zA-Z0-9]/g, "");
  const [svg, setSvg] = useState<string | null>(null);
  const [err, setErr] = useState(false);
  const appearance = useAppearance();
  useEffect(() => {
    let live = true;
    (async () => {
      try {
        const m = (await import("mermaid")).default;
        m.initialize(mermaidConfig(appearance));
        const r = await m.render(id, source);
        if (live) {
          setSvg(r.svg);
          setErr(false);
        }
      } catch {
        document.getElementById("d" + id)?.remove(); // mermaid leaves an error node in <body>
        if (live) setErr(true);
      }
    })();
    return () => {
      live = false;
    };
  }, [source, id, appearance]);
  if (err) return <pre className="overflow-auto rounded bg-accent p-2 text-xs">{source}</pre>;
  if (!svg) return <div className="p-4 text-xs text-muted-foreground" role="status">Disegno il diagramma…</div>;
  return <div className={`overflow-auto [&_svg]:mx-auto [&_svg]:h-auto [&_svg]:max-w-full ${className}`} dangerouslySetInnerHTML={{ __html: svg }} />;
}
