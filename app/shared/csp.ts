// Content-Security-Policy for subagent demos: inline code only, no network of any kind. Used both by the headless
// test run (app/server/jobs/render.ts) and by the UI iframe (app/web/src/session/cards/JobCard.tsx), so what was
// tested is what ships.
export const DEMO_CSP =
  "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; " +
  "media-src data: blob:; connect-src 'none'; form-action 'none'; frame-src 'none'; worker-src 'none'";

/** The demo HTML with the CSP meta inserted right after a leading doctype (before it would trigger quirks mode);
 *  a <meta> CSP applies to everything parsed after it. */
export function withDemoCsp(html: string): string {
  const meta = `<meta http-equiv="Content-Security-Policy" content="${DEMO_CSP}">`;
  const m = /^\s*<!doctype[^>]*>/i.exec(html);
  return m ? m[0] + meta + html.slice(m[0].length) : meta + html;
}

/** Sandbox for LLM-authored lesson files served under /files; the Rust twin is
 *  crates/proxi/src/files.rs::LESSON_FILE_CSP. */
export const LESSON_FILE_CSP = "sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox; frame-ancestors 'self'";
