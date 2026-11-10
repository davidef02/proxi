// Adapted from LearnHarness (github.com/amosblomqvist/learnharness), AGPL-3.0-only.
import { useEffect, type RefObject } from "react";

/** Close a floating panel on any pointerdown outside its root, or on Escape. */
export function usePopoverDismiss(rootRef: RefObject<HTMLElement | null>, open: boolean, close: () => void): void {
	useEffect(() => {
		if (!open) return;
		const onPointerDown = (e: PointerEvent) => {
			if (rootRef.current && !rootRef.current.contains(e.target as Node)) close();
		};
		const onKeyDown = (e: KeyboardEvent) => {
			if (e.key === "Escape") {
				e.stopPropagation();
				close();
			}
		};
		document.addEventListener("pointerdown", onPointerDown);
		document.addEventListener("keydown", onKeyDown);
		return () => {
			document.removeEventListener("pointerdown", onPointerDown);
			document.removeEventListener("keydown", onKeyDown);
		};
	}, [open, close, rootRef]);
}
