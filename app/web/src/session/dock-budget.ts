/**
 * Pure mathematical height budgeting and scroll event forwarding for the session dock.
 * Ensures short windows and small screens preserve a guaranteed viewing slice for the lesson transcript.
 */

export interface DockBudgetInputs {
  /** Height of the column enclosing Chat + Dock (px) */
  columnHeight: number;
  /** Guaranteed minimum viewing slice for Chat (px, default 140 or 25% of column) */
  minTranscriptHeight?: number;
  /** Vertical gap/padding outside the dock shell (px, default 20) */
  dockGaps?: number;
  /** Measured height of the pinned dock footer (px) */
  footerHeight: number;
  /** Internal padding height of the body/question (px, default 24) */
  bodyPaddingHeight?: number;
  /** Natural total unconstrained height of dock children */
  naturalContentHeight?: number;
}

export interface DockBudget {
  /** Maximum height allowed for the entire Shell */
  shellMaxHeight: number;
  /** Maximum height allowed for the scrollable body */
  bodyMaxHeight: number;
  /** Guaranteed height preserved for the transcript */
  transcriptSlice: number;
  /** True when viewport is too short for split footer/body scrolling */
  scrollWholeContent: boolean;
}

/**
 * Computes a balanced height budget between the lesson transcript and the interactive dock.
 */
export function computeDockBudget(inputs: DockBudgetInputs): DockBudget {
  const minTranscript =
    inputs.minTranscriptHeight ?? Math.max(140, Math.round(inputs.columnHeight * 0.25));
  const dockGaps = inputs.dockGaps ?? 20;
  const bodyPadding = inputs.bodyPaddingHeight ?? 24;

  // Maximum space the dock is permitted to consume in the column
  const maxDockSpace = Math.max(inputs.columnHeight - minTranscript - dockGaps, 0);

  // Height available for the scrollable body after subtracting footer and borders (2px)
  const bodyMaxHeight = Math.max(maxDockSpace - inputs.footerHeight - 2, 0);

  // When body space is less than or equal to internal padding, split scrolling is broken:
  // fall back to scrolling the whole dock content together.
  const scrollWholeContent = bodyMaxHeight <= bodyPadding;

  const naturalHeight = inputs.naturalContentHeight ?? bodyMaxHeight;
  const shellMaxHeight = scrollWholeContent
    ? maxDockSpace
    : Math.min(naturalHeight + inputs.footerHeight + 2, maxDockSpace);

  const transcriptSlice = Math.max(inputs.columnHeight - shellMaxHeight - dockGaps, 0);

  return {
    shellMaxHeight,
    bodyMaxHeight,
    transcriptSlice,
    scrollWholeContent,
  };
}

const isHtmlElement = (el: unknown): el is HTMLElement =>
  typeof HTMLElement !== "undefined" ? el instanceof HTMLElement : Boolean(el && typeof el === "object" && "parentElement" in el);

/**
 * Smoothly forwards mouse-wheel scroll events from the dock to the transcript
 * when the dock is not scrollable or has reached its scroll boundary.
 */
export function forwardDockWheel(
  event: WheelEvent,
  dockRoot: HTMLElement,
  transcript: HTMLElement,
): void {
  if (event.deltaY === 0) return;

  // Walk up from event.target to dockRoot to check if an inner scroll container can scroll
  for (let n = event.target as HTMLElement | null; n && n !== dockRoot; n = n.parentElement) {
    if (!isHtmlElement(n)) continue;
    const overflowY =
      (n.style && n.style.overflowY) ||
      (typeof window !== "undefined" && window.getComputedStyle ? window.getComputedStyle(n).overflowY : "");
    if (!/^(auto|scroll)$/.test(overflowY)) continue;
    if (n.scrollHeight - n.clientHeight < 2) continue;

    // Check if scrolling up and not at top, or scrolling down and not at bottom
    if (event.deltaY < 0 ? n.scrollTop > 0 : n.scrollTop < n.scrollHeight - n.clientHeight - 0.5) {
      return; // Element consumes the wheel scroll natively
    }
  }

  // At scroll edge or non-scrollable region: forward to transcript
  if (typeof event.preventDefault === "function") {
    event.preventDefault();
  }
  const multiplier = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? transcript.clientHeight || 400 : 1;
  transcript.scrollTop += event.deltaY * multiplier;
}
