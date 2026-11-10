// CodeMirror 6 editor, remounted per file (key=path). `value` is only the initial text: the parent keeps the live text via onChange.
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { css } from "@codemirror/lang-css";
import { html } from "@codemirror/lang-html";
import { javascript } from "@codemirror/lang-javascript";
import { vue } from "@codemirror/lang-vue";
import { HighlightStyle, bracketMatching, indentOnInput, syntaxHighlighting } from "@codemirror/language";
import { Compartment, EditorState, Prec, type Extension } from "@codemirror/state";
import { EditorView, drawSelection, highlightActiveLine, keymap, lineNumbers } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import { useEffect, useRef } from "react";
import { useMode } from "../theme.ts";

const spec = {
  "&": { height: "100%", fontVariantLigatures: "none", backgroundColor: "var(--background)", color: "var(--foreground)", fontSize: "13px" },
  ".cm-scroller": { fontFamily: "var(--font-mono)", lineHeight: "1.6" },
  ".cm-content": { caretColor: "var(--primary)" },
  ".cm-cursor": { borderLeftColor: "var(--primary)" },
  ".cm-gutters": { backgroundColor: "var(--background)", color: "var(--muted-foreground)", border: "none" },
  ".cm-activeLine": { backgroundColor: "color-mix(in srgb, var(--accent) 55%, transparent)" },
  ".cm-activeLineGutter": { backgroundColor: "transparent", color: "var(--foreground)" },
  "&.cm-focused": { outline: "none" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": { backgroundColor: "var(--border)" },
};
// Colours come from the CSS tokens; the dark flag only picks CodeMirror's own base defaults, swapped live.
const themes = { dark: EditorView.theme(spec, { dark: true }), light: EditorView.theme(spec, { dark: false }) };

const highlight = HighlightStyle.define([
  { tag: [t.keyword, t.controlKeyword, t.definitionKeyword, t.moduleKeyword], color: "var(--syn-keyword)" },
  { tag: [t.string, t.special(t.string)], color: "var(--good)" },
  { tag: [t.number, t.bool, t.null, t.atom], color: "var(--warn)" },
  { tag: [t.comment, t.lineComment, t.blockComment], color: "var(--muted-foreground)", fontStyle: "italic" },
  { tag: [t.function(t.variableName), t.function(t.propertyName), t.propertyName], color: "var(--info)" },
  { tag: [t.tagName, t.typeName, t.className], color: "var(--bad)" },
  { tag: [t.attributeName, t.operator], color: "var(--syn-attr)" },
]);

function language(path: string): Extension {
  if (path.endsWith(".vue")) return vue();
  if (path.endsWith(".css")) return css();
  if (path.endsWith(".html")) return html();
  if (path.endsWith(".ts")) return javascript({ typescript: true });
  if (/\.(js|json)$/.test(path)) return javascript();
  return [];
}

export function CodeEditor({ path, value, readOnly, onChange }: { path: string; value: string; readOnly: boolean; onChange: (v: string) => void }) {
  const host = useRef<HTMLDivElement>(null);
  const cb = useRef(onChange);
  cb.current = onChange;
  const mode = useMode();
  const view = useRef<EditorView>(null);
  const themeSlot = useRef(new Compartment());

  useEffect(() => {
    const v = new EditorView({
      parent: host.current!,
      state: EditorState.create({
        doc: value,
        extensions: [
          // Mod-Enter runs the tests (window listener in ExercisePage); swallow it here so it does not insert a line
          Prec.highest(keymap.of([{ key: "Mod-Enter", run: () => true }])),
          lineNumbers(), EditorView.lineWrapping, history(), drawSelection(), indentOnInput(), bracketMatching(), highlightActiveLine(),
          keymap.of([...defaultKeymap, ...historyKeymap, indentWithTab]),
          language(path), themeSlot.current.of(themes[mode]), syntaxHighlighting(highlight),
          EditorState.readOnly.of(readOnly), EditorView.editable.of(!readOnly),
          EditorView.updateListener.of((u) => u.docChanged && cb.current(u.state.doc.toString())),
        ],
      }),
    });
    view.current = v;
    return () => v.destroy();
    // initial text only: remounted per file via key
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path, readOnly]);

  useEffect(() => view.current?.dispatch({ effects: themeSlot.current.reconfigure(themes[mode]) }), [mode]);

  return <div ref={host} className="min-h-0 flex-1 overflow-hidden" data-testid="code-editor" />;
}
