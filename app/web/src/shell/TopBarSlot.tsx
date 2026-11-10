// The top bar's middle area belongs to the page. A page fills it by rendering <TopBarSlot>…</TopBarSlot> anywhere in
// its tree; the children are portalled into the bar and removed when the page unmounts. Buttons/inputs inside are
// automatically `no-drag` (see styles.css `.topbar`). Without a filled slot the bar shows the default route title.
import { createContext, useContext, useEffect, type ReactNode } from "react";
import { createPortal } from "react-dom";

export interface SlotCtx {
  /** The DOM node inside the top bar (null before first mount). */
  el: HTMLElement | null;
  /** Number of mounted <TopBarSlot>s: the bar hides its default title when > 0. */
  count: number;
  bump(d: 1 | -1): void;
}
export const TopBarSlotContext = createContext<SlotCtx>({ el: null, count: 0, bump: () => {} });

export function TopBarSlot({ children }: { children: ReactNode }) {
  const { el, bump } = useContext(TopBarSlotContext);
  useEffect(() => {
    bump(1);
    return () => bump(-1);
  }, [bump]);
  return el ? createPortal(children, el) : null;
}
