import { useEffect, useLayoutEffect, useState, type RefObject } from "react";
import { computeDockBudget, forwardDockWheel, type DockBudget } from "./dock-budget.ts";

const useIsoLayoutEffect = typeof window !== "undefined" ? useLayoutEffect : useEffect;

/**
 * Observes container, dock shell, body, and footer sizes, calculates dynamic height budgets,
 * sets CSS variables (`--dock-max-h`, `--dock-body-max-h`), and forwards wheel scroll to transcript.
 */
export function useDockHeight(
  rootRef: RefObject<HTMLElement | null>,
  bodyRef: RefObject<HTMLElement | null>,
  footerRef: RefObject<HTMLElement | null>,
): DockBudget | null {
  const [budget, setBudget] = useState<DockBudget | null>(null);

  useIsoLayoutEffect(() => {
    const root = rootRef.current;
    if (!root || typeof window === "undefined") return;

    // Chat column containing the transcript and dock
    const column =
      root.closest<HTMLElement>("section[aria-label='Chat']") ??
      root.parentElement?.parentElement ??
      root.parentElement;
    if (!column) return;

    const measure = () => {
      const colHeight = column.clientHeight || window.innerHeight;
      const footerH = footerRef.current?.offsetHeight ?? 48;
      const naturalContentH = bodyRef.current?.scrollHeight ?? 200;

      const b = computeDockBudget({
        columnHeight: colHeight,
        footerHeight: footerH,
        naturalContentHeight: naturalContentH,
      });

      root.style.setProperty("--dock-max-h", `${b.shellMaxHeight}px`);
      root.style.setProperty("--dock-body-max-h", `${b.bodyMaxHeight}px`);
      setBudget(b);
    };

    measure();

    if (typeof ResizeObserver !== "undefined") {
      const ro = new ResizeObserver(measure);
      ro.observe(column);
      ro.observe(root);
      if (bodyRef.current) ro.observe(bodyRef.current);
      if (footerRef.current) ro.observe(footerRef.current);
      return () => ro.disconnect();
    } else {
      window.addEventListener("resize", measure);
      return () => window.removeEventListener("resize", measure);
    }
  }, []);

  // Forward wheel events from the dock to the transcript when at scroll edges
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;

    const onWheel = (e: WheelEvent) => {
      const transcript = document.querySelector<HTMLElement>("[data-chat-scroll]");
      if (!transcript) return;
      forwardDockWheel(e, root, transcript);
    };

    root.addEventListener("wheel", onWheel, { passive: false });
    return () => root.removeEventListener("wheel", onWheel);
  }, []);

  return budget;
}
