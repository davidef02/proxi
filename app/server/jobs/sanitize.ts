// Trust boundary for subagent output. SVG is shown in the UI as an <img> data URL and rendered with JS off; demo HTML
// runs only in a sandboxed iframe under DEMO_CSP (app/shared/csp.ts), which is also what the headless test run uses.
// The SVG check is a whitelist of elements plus a per-attribute inspection (not a regex over the whole text); the demo
// check is only an early-feedback hint, the CSP is the real network boundary.
// Both are strict: a false rejection costs the subagent one retry, a false accept costs the learner.

const MAX_SVG = 200_000;
const MAX_HTML = 500_000;

/** Entities decoded (`&#106;avascript:`), control chars and CSS escapes removed, so obfuscation cannot hide a scheme. */
function decode(s: string): string {
  const cp = (n: number) => (n > 0 && n <= 0x10ffff ? String.fromCodePoint(n) : "");
  return s
    .replace(/&#x([0-9a-f]+);?/gi, (_, h) => cp(parseInt(h, 16)))
    .replace(/&#(\d+);?/g, (_, d) => cp(parseInt(d, 10)))
    .replace(/&colon;/gi, ":")
    .replace(/&(tab|newline);/gi, "")
    .replace(/[\u0000-\u001f\u007f]+/g, "") // browsers drop tab/CR/LF inside URLs: strip, never turn into a space
    .replace(/\\([0-9a-f]{1,6})\s?/gi, (_, h) => cp(parseInt(h, 16))) // CSS escapes: u\72l(
    .replace(/\\(.)/g, "$1");
}

const TAG = /<(\/?)([a-zA-Z][^\s\/>]*)((?:"[^"]*"|'[^']*'|[^>"'])*)>/g;
const ATTR = /([^\s=\/>"']+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>"']+)))?/g;
const CSS_URL = /url\s*\(\s*(['"]?)([^)'"]*)\1\s*\)/gi;

/** [tag name lowercased, attributes] of every tag in `s`. */
function tags(s: string): { name: string; closing: boolean; attrs: [string, string][] }[] {
  return [...s.matchAll(TAG)].map((m) => ({
    name: m[2].toLowerCase(),
    closing: !!m[1],
    attrs: [...m[3].matchAll(ATTR)].map((a): [string, string] => [a[1].toLowerCase(), a[2] ?? a[3] ?? a[4] ?? ""]),
  }));
}

const SVG_ELEMENTS = new Set(
  ("svg g defs symbol use path rect circle ellipse line polyline polygon text tspan textpath title desc metadata marker " +
    "lineargradient radialgradient stop clippath mask pattern style filter fegaussianblur feoffset feblend fecolormatrix " +
    "feflood fecomposite femerge femergenode fedropshadow fecomponenttransfer fefunca fefuncr fefuncg fefuncb femorphology " +
    "feturbulence fediffuselighting fespecularlighting fedistantlight fepointlight fespotlight fetile feimage a switch").split(" "),
);
/** Attributes that carry a URL: only a same-document `#id` is allowed. */
const URL_ATTRS = new Set(["href", "xlink:href"]);
/** Attributes that never make sense in a figure and are classic carriers of external loads. */
const BANNED_ATTRS = new Set(["src", "srcset", "background", "action", "formaction", "poster", "ping", "data", "style-src"]);

function checkCss(css: string, errs: string[]): void {
  const c = decode(css);
  if (/@import/i.test(c)) errs.push("@import non ammesso");
  if (/image-set\s*\(|(?:^|[^\w-])src\s*\(|cross-fade\s*\(|expression\s*\(|behavior\s*:/i.test(c)) errs.push("image-set()/src()/expression() non ammessi in CSS");
  for (const m of c.matchAll(CSS_URL)) if (!m[2].trim().startsWith("#")) errs.push(`url() esterno non ammesso: ${m[0].slice(0, 60)} (solo url(#id))`);
}

export function checkSvg(svg: string): string[] {
  const errs: string[] = [];
  if (svg.length > MAX_SVG) return [`SVG troppo grande (${svg.length} caratteri, massimo ${MAX_SVG})`];
  const body = svg.replace(/^\s*(<\?xml[^>]*\?>\s*)?/i, "").replace(/<!--[\s\S]*?-->/g, "").trim();
  if (!/^<svg[\s>\/]/i.test(body) || !/<\/svg\s*>\s*$|\/>\s*$/i.test(body)) errs.push("la radice deve essere un elemento <svg> e il documento deve chiudersi con </svg>");
  if (/<!\s*(doctype|entity)/i.test(svg)) errs.push("DOCTYPE/ENTITY non ammessi");
  if (/<!\[CDATA\[|<\?(?!xml)/i.test(body)) errs.push("CDATA e istruzioni di elaborazione non ammessi");
  const all = tags(body);
  for (const t of all) {
    if (!SVG_ELEMENTS.has(t.name)) {
      errs.push(`elemento <${t.name}> non ammesso`);
      continue;
    }
    for (const [name, raw] of t.attrs) {
      const v = decode(raw).trim();
      if (name.startsWith("on")) errs.push("attributi di evento on*= non ammessi");
      else if (BANNED_ATTRS.has(name)) errs.push(`attributo ${name}= non ammesso`);
      else if (URL_ATTRS.has(name) && !v.startsWith("#")) errs.push(`riferimento esterno non ammesso: ${name}="${raw.slice(0, 50)}" (solo href="#id")`);
      else if (name === "style") checkCss(raw, errs);
      if (/(?:java|vb)script:|data:text\/html/i.test(v)) errs.push("URL javascript:/data:text/html non ammessi");
      if (name !== "style") for (const m of v.matchAll(CSS_URL)) if (!m[2].trim().startsWith("#")) errs.push(`url() esterno non ammesso: ${m[0].slice(0, 60)} (solo url(#id))`);
    }
  }
  for (const m of body.matchAll(/<style\b[^>]*>([\s\S]*?)<\/style\s*>/gi)) checkCss(m[1], errs);
  if (/<\s*style\b/i.test(body) && !/<\/style\s*>/i.test(body)) errs.push("<style> non chiuso");
  return [...new Set(errs)];
}

/** Early feedback only (cheap, bypassable by design): the real boundary is the CSP the page runs under. */
export function checkDemo(html: string): string[] {
  const errs: string[] = [];
  if (!html.trim()) return ["HTML vuoto"];
  if (html.length > MAX_HTML) return [`HTML troppo grande (${html.length} caratteri, massimo ${MAX_HTML})`];
  const remote = (v: string) => /^\s*(https?:)?\/\//i.test(v) || /^\s*(ftp|ws|wss|file):/i.test(v);
  for (const t of tags(html)) {
    if (["iframe", "object", "embed", "base"].includes(t.name)) errs.push("<iframe>/<object>/<embed>/<base> non ammessi");
    for (const [name, raw] of t.attrs) {
      const v = decode(raw);
      if (["href", "xlink:href", "src", "srcset", "action", "formaction", "poster", "background", "ping", "data"].includes(name) && remote(v))
        errs.push(`risorsa di rete esterna non ammessa: ${name}="${raw.slice(0, 60)}"`);
      if (name === "http-equiv" && /refresh/i.test(v)) errs.push("<meta refresh> non ammesso");
    }
  }
  const css = decode(html);
  for (const m of css.matchAll(CSS_URL)) if (remote(m[2])) errs.push(`url() esterno non ammesso: ${m[0].slice(0, 80)}`);
  if (/@import\s+(url\()?\s*['"]?\s*(https?:)?\/\//i.test(css)) errs.push("@import esterno non ammesso");
  const lit = String.raw`\s*\(\s*[\`'"]\s*(?:https?:|wss?:)?\/\/`;
  if (new RegExp(String.raw`\b(?:fetch|WebSocket|EventSource|sendBeacon|importScripts|import)${lit}`, "i").test(html) ||
      new RegExp(String.raw`\.open\s*\(\s*[\`'"][A-Za-z]+[\`'"]\s*,\s*[\`'"]\s*(?:https?:)?\/\/`).test(html))
    errs.push("chiamate di rete verso URL remoti non ammesse (fetch/XMLHttpRequest/WebSocket/import)");
  if (/\.(?:src|href)\s*=\s*[`'"]\s*(?:https?:)?\/\//i.test(html)) errs.push("assegnazione di src/href remoti non ammessa");
  return [...new Set(errs)];
}
