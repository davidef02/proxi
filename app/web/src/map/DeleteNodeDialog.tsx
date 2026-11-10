// Adapted from LearnHarness (github.com/amosblomqvist/learnharness), AGPL-3.0-only.
import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { mapApi } from "./api.ts";

/** Confirm + delete one or many propositions (cascades to every chain that uses them, server-side). */
export function DeleteNodeDialog({
	nodes,
	onClose,
	onDeleted,
	onPartial,
}: {
	nodes: { id: string; name: string }[] | null;
	onClose: () => void;
	/** Every node is gone: the parent closes the dialog. */
	onDeleted: (nodeIds: string[]) => void;
	/** Some nodes are gone but one failed: the dialog stays open showing the error, the parent only refreshes. */
	onPartial?: (nodeIds: string[]) => void;
}) {
	const [busy, setBusy] = useState(false);
	const [failure, setFailure] = useState<string | null>(null);
	const [gone, setGone] = useState<Set<string>>(new Set());

	useEffect(() => {
		if (!nodes || nodes.length === 0) {
			setBusy(false);
			setFailure(null);
			setGone(new Set());
		}
	}, [nodes]);

	useEffect(() => {
		if (!nodes || nodes.length === 0) return;
		const onKeyDown = (e: KeyboardEvent) => {
			if (e.key === "Escape") {
				e.preventDefault();
				e.stopPropagation();
				onClose();
			}
		};
		document.addEventListener("keydown", onKeyDown);
		return () => document.removeEventListener("keydown", onKeyDown);
	}, [nodes, onClose]);

	if (!nodes || nodes.length === 0) return null;

	const run = async () => {
		if (busy) return;
		setBusy(true);
		setFailure(null);
		const dead: string[] = [];
		try {
			for (const n of nodes) {
				if (gone.has(n.id)) continue; // already deleted by an earlier attempt
				await mapApi.deleteNode(n.id); // sequential, never parallel
				dead.push(n.id);
			}
			onDeleted([...gone, ...dead]);
		} catch (e) {
			setFailure((e as Error).message || "Eliminazione non riuscita.");
			setBusy(false);
			if (dead.length > 0) {
				setGone((g) => new Set([...g, ...dead]));
				onPartial?.(dead);
			}
		}
	};

	return createPortal(
		<div
			className="fixed inset-0 z-50 flex items-center justify-center bg-background/80"
			onClick={(e) => {
				if (e.target === e.currentTarget && !busy) onClose();
			}}
		>
			<div
				role="dialog"
				aria-modal="true"
				aria-label="Elimina nodo"
				data-testid="delete-node-dialog"
				className="w-[32rem] max-w-[calc(100vw-2rem)] rounded-xl border bg-card p-5 shadow-xl"
			>
				<h2 className="text-sm font-semibold text-bad">{nodes.length === 1 ? "Eliminare il nodo?" : `Eliminare ${nodes.length} nodi?`}</h2>
				<div className="mt-1.5">
					<code className="font-mono text-sm text-foreground">
						{nodes
							.slice(0, 8)
							.map((n) => n.name)
							.join(", ")}
						{nodes.length > 8 ? `, e altri ${nodes.length - 8}` : ""}
					</code>
				</div>
				<p className="mt-2 text-sm text-muted-foreground">
					{nodes.length === 1
						? "Il nodo e le catene che lo usano vengono eliminati in modo definitivo."
						: `I ${nodes.length} nodi e le catene che li usano vengono eliminati in modo definitivo.`}
				</p>
				{failure && (
					<div role="alert" className="mt-3 rounded-sm border border-border/60 px-3 py-2 text-xs text-foreground">
						{failure}
					</div>
				)}
				<div className="mt-4 flex justify-end gap-2">
					<button type="button" onClick={onClose} disabled={busy} className="rounded-sm px-3 py-1.5 text-xs text-muted-foreground transition-colors hover:text-foreground disabled:opacity-40">
						Annulla
					</button>
					<button
						type="button"
						data-testid="node-delete-confirm"
						onClick={() => void run()}
						disabled={busy}
						className="rounded-sm border border-bad/50 bg-bad/10 px-3 py-1.5 text-xs text-bad transition-colors hover:bg-bad/20 disabled:opacity-40"
					>
						{busy ? "Elimino…" : "Elimina"}
					</button>
				</div>
			</div>
		</div>,
		document.body,
	);
}
