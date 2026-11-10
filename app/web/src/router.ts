// Hash router: #/ (map)  ·  #/c/<course>  ·  #/s/<sessionId>  ·  #/l/<lesson path>  ·  #/r/<reference path>  ·  #/e/<course>/<exercise>
import { useEffect, useState } from "react";

export type Route =
  | { name: "home" }
  | { name: "course"; slug: string }
  | { name: "session"; id: string }
  /** In-app lesson reader; path is repo-relative (courses/<c>/lessons/NNNN-x.html), node = optional anchor. */
  | { name: "lesson"; path: string; node?: string }
  /** In-app reference sheet reader; path is repo-relative (courses/<c>/reference/x.html). */
  | { name: "reference"; path: string }
  /** In-app exercise workspace (editor + tests + hints). */
  | { name: "exercise"; course: string; exercise: string; sessionId?: string };

export function parseRoute(hash: string): Route {
  const h = hash.replace(/^#/, "");
  const l = h.match(/^\/l\/([^#?]+?)(?:#node-([\w-]+))?$/);
  if (l) return { name: "lesson", path: decodeURIComponent(l[1]), ...(l[2] ? { node: l[2] } : {}) };
  const r = h.match(/^\/r\/([^#?]+)$/);
  if (r) return { name: "reference", path: decodeURIComponent(r[1]) };
  const e = h.match(/^\/e\/([\w-]+)\/([\w-]+)(?:\?s=([\w-]+))?/);
  if (e) return { name: "exercise", course: e[1], exercise: e[2], ...(e[3] ? { sessionId: e[3] } : {}) };
  const s = h.match(/^\/s\/([\w-]+)/);
  if (s) return { name: "session", id: s[1] };
  const c = h.match(/^\/c\/([\w-]+)/);
  if (c) return { name: "course", slug: c[1] };
  return { name: "home" };
}

export const lessonHref = (path: string, node?: string) => `#/l/${encodeURIComponent(path)}${node ? `#node-${node}` : ""}`;
export const referenceHref = (path: string) => `#/r/${encodeURIComponent(path)}`;
export const exerciseHref = (course: string, exercise: string, sessionId?: string) => `#/e/${course}/${exercise}${sessionId ? `?s=${sessionId}` : ""}`;

export function navigate(to: string): void {
  location.hash = to;
}

export function useRoute(): Route {
  const [route, setRoute] = useState(() => parseRoute(location.hash));
  useEffect(() => {
    const on = () => setRoute(parseRoute(location.hash));
    addEventListener("hashchange", on);
    return () => removeEventListener("hashchange", on);
  }, []);
  return route;
}
