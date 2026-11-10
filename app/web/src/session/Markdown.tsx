import { Children, cloneElement, isValidElement, type ComponentProps } from "react";
import { Streamdown } from "streamdown";
import { code } from "@streamdown/code";
import { createMathPlugin } from "@streamdown/math";
import { mermaid } from "@streamdown/mermaid";
import { mermaidConfig } from "./Mermaid.tsx";
import { useAppearance } from "../theme.ts";

// singleDollarTextMath: the teacher writes inline math as $...$ (off by default in the plugin).
const plugins = { code, math: createMathPlugin({ singleDollarTextMath: true }), mermaid };
const remend = { htmlTags: false };

const CALLOUT = { NOTE: "Nota", TIP: "Suggerimento", IMPORTANT: "Importante", WARNING: "Attenzione" } as const;

/** GitHub-style `> [!NOTE]` quotes become quiet callouts; any other quote stays a quote. */
function Blockquote({ children, node: _node, ...rest }: ComponentProps<"blockquote"> & { node?: unknown }) {
  const kids = Children.toArray(children);
  const at = kids.findIndex(isValidElement);
  const p = kids[at];
  const inner = isValidElement<{ children?: React.ReactNode }>(p) ? Children.toArray(p.props.children) : [];
  const m = typeof inner[0] === "string" ? inner[0].match(/^\s*\[!(NOTE|TIP|IMPORTANT|WARNING)\][ \t]*\n?/) : null;
  if (!m || !isValidElement(p)) return <blockquote {...rest}>{children}</blockquote>;
  const kind = m[1] as keyof typeof CALLOUT;
  kids[at] = cloneElement(p, undefined, (inner[0] as string).slice(m[0].length), ...inner.slice(1));
  return (
    <div className="callout" data-kind={kind.toLowerCase()}>
      <p className="callout-title">{CALLOUT[kind]}</p>
      {kids}
    </div>
  );
}

const components = { blockquote: Blockquote };

export function Markdown({ text, streaming = false, className = "" }: { text: string; streaming?: boolean; className?: string }) {
  const appearance = useAppearance();
  return (
    <div className={`prose-lh ${className}`}>
      <Streamdown
        mode={streaming ? "streaming" : "static"}
        parseIncompleteMarkdown={streaming}
        isAnimating={streaming}
        plugins={plugins}
        components={components}
        mermaid={{ config: mermaidConfig(appearance) }}
        remend={remend}
      >
        {text}
      </Streamdown>
    </div>
  );
}
