// Adapted from LearnHarness (github.com/amosblomqvist/learnharness), AGPL-3.0-only.
/**
 * The map: one Canvas2D renderer (graph-canvas.ts) fed with the server snapshot. This component does everything
 * around drawing: fetch + live refetch, layout fallback, search, detail card, settings popover, chain table,
 * practice selection, "Aggiorna mappa". Dropped from LearnHarness: notes import, cost summary, physics tuning panel,
 * rename events, keyboard-shortcut registry.
 */
import { useDeferredValue, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { ListTreeIcon, Loader2Icon, RefreshCwIcon, SearchIcon, SettingsIcon, WaypointsIcon } from "lucide-react";
import { needsMapping, type GlobalEvent, type GraphSource, type JobRecord } from "../../../shared/protocol.ts";
import {
	LABEL_SIZE_LADDER,
	LABEL_ZOOM_LADDER,
	NODE_SIZE_LADDER,
	fromLadderPosition,
	birthSessionOf,
	keyConcepts,
	layeredLayout,
	labelZoomDisplay,
	restoreRenderSettings,
	sparseRenderSettings,
	toLadderPosition,
	type GraphNode,
	type GraphRenderSettings,
	type GraphSnapshot,
} from "../../../shared/graph/index.ts";
import { GraphCanvasRenderer } from "./graph-canvas.ts";
import { ChainTableOverlay } from "./ChainTable.tsx";
import { DeleteNodeDialog } from "./DeleteNodeDialog.tsx";
import { courseOf, lessonUrl, mapApi, subscribeEvents } from "./api.ts";
import { usePopoverDismiss } from "./dismiss.ts";
import { useAppearance } from "../theme.ts";

const CAMERA_KEY = "ia.map.camera.v2"; // v2: layered layout; a camera saved on the old free layout would frame nothing
const SETTINGS_KEY = "ia.map.settings.v3"; // stored sparse (only fields that differ from the defaults, #41): a changed default reaches every profile, so never bump this key

/** Search filter: empty query shows everything, otherwise name or statement contains the query. */
export function isNodeVisible(node: Pick<GraphNode, "name" | "statement">, query: string): boolean {
	const q = query.trim().toLowerCase();
	if (!q) return true;
	return node.name.toLowerCase().includes(q) || node.statement.toLowerCase().includes(q);
}

/** Nodes the positions ledger does not know yet go on a deterministic ring around the known ones. */
export function seedMissingNodePositions(
	nodes: readonly { id: string }[],
	positions: Map<string, { x: number; y: number }>,
): Map<string, { x: number; y: number }> {
	if (positions.size === 0 || nodes.every((n) => positions.has(n.id))) return positions;
	let minX = Infinity,
		minY = Infinity,
		maxX = -Infinity,
		maxY = -Infinity;
	for (const p of positions.values()) {
		minX = Math.min(minX, p.x);
		minY = Math.min(minY, p.y);
		maxX = Math.max(maxX, p.x);
		maxY = Math.max(maxY, p.y);
	}
	const cx = (minX + maxX) / 2;
	const cy = (minY + maxY) / 2;
	const radius = Math.max(maxX - minX, maxY - minY, 40) * 0.7 + 20;
	nodes
		.filter((n) => !positions.has(n.id))
		.forEach((n, i) => {
			let h = 0x811c9dc5;
			for (let c = 0; c < n.id.length; c++) {
				h ^= n.id.charCodeAt(c);
				h = (h * 0x01000193) >>> 0;
			}
			const angle = ((h / 0xffffffff) * 2 * Math.PI + i * 2.39996323) % (2 * Math.PI);
			positions.set(n.id, { x: cx + Math.cos(angle) * radius, y: cy + Math.sin(angle) * radius });
		});
	return positions;
}

const cssVar = (name: string, fallback: string) => getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;

const sizeDisplay = (v: number) => `${v >= 1 ? Math.round(v * 10) / 10 : Math.round(v * 100) / 100}`;

function SettingSlider({
	label,
	ladder,
	value,
	display,
	onChange,
}: {
	label: string;
	ladder: readonly number[];
	value: number;
	display?: string;
	onChange: (v: number) => void;
}) {
	return (
		<label className="flex flex-col gap-1">
			<span className="flex items-center justify-between text-[11px] text-muted-foreground">
				{label}
				<span className="tabular-nums">{display ?? sizeDisplay(value)}</span>
			</span>
			<input
				type="range"
				min={0}
				max={100}
				step={10}
				value={toLadderPosition(value, ladder)}
				onChange={(e) => onChange(fromLadderPosition(Number(e.target.value), ladder))}
				className="h-1.5 w-full accent-primary"
			/>
		</label>
	);
}

type Change = (update: (cur: GraphRenderSettings) => GraphRenderSettings) => void;

function GraphSettingsControl({ settings, change }: { settings: GraphRenderSettings; change: Change }) {
	const [open, setOpen] = useState(false);
	const rootRef = useRef<HTMLDivElement>(null);
	usePopoverDismiss(rootRef, open, () => setOpen(false));
	return (
		<div ref={rootRef} className="relative">
			<button
				type="button"
				title="Impostazioni della mappa"
				aria-label="Impostazioni della mappa"
				aria-expanded={open}
				onClick={() => setOpen((o) => !o)}
				className={`flex size-9 items-center justify-center rounded-xl border bg-card/85 shadow-lg backdrop-blur transition-colors hover:bg-accent ${
					open ? "text-foreground" : "text-muted-foreground hover:text-foreground"
				}`}
			>
				<SettingsIcon className="size-4" />
			</button>
			{open && (
				<div
					data-testid="map-settings-panel"
					className="absolute right-0 top-full mt-2 flex max-h-[60vh] w-72 flex-col rounded-xl border bg-card/95 p-2.5 shadow-lg backdrop-blur"
				>
					<div className="flex flex-col gap-2 overflow-y-auto pr-2.5">
						<SettingSlider label="Dimensione nodi" ladder={NODE_SIZE_LADDER} value={settings.nodeSize} onChange={(v) => change((c) => ({ ...c, nodeSize: v }))} />
						<SettingSlider label="Dimensione etichette" ladder={LABEL_SIZE_LADDER} value={settings.labelSize} onChange={(v) => change((c) => ({ ...c, labelSize: v }))} />
						<SettingSlider
							label="Etichette fino allo zoom"
							ladder={LABEL_ZOOM_LADDER}
							value={settings.labelZoom}
							display={labelZoomDisplay(settings.labelZoom)}
							onChange={(v) => change((c) => ({ ...c, labelZoom: v }))}
						/>
						<label className="flex cursor-pointer items-center justify-between text-[11px] text-muted-foreground">
							<span>Colori di sfondo</span>
							<input
								type="checkbox"
								checked={settings.regions.enabled}
								onChange={(e) => change((c) => ({ ...c, regions: { ...c.regions, enabled: e.target.checked } }))}
								className="accent-primary"
							/>
						</label>
						<label className="flex cursor-pointer items-center justify-between text-[11px] text-muted-foreground">
							<span>Etichette delle lezioni</span>
							<input
								type="checkbox"
								checked={settings.clusterLabels}
								onChange={(e) => change((c) => ({ ...c, clusterLabels: e.target.checked }))}
								className="accent-primary"
							/>
						</label>
					</div>
				</div>
			)}
		</div>
	);
}

interface Selection {
	node: GraphNode;
	sourceIds: string[];
	/** Node position in container pixels at click time (a camera move dismisses the card). */
	anchor: { x: number; y: number };
}

function loadSettings(): GraphRenderSettings {
	try {
		const raw = localStorage.getItem(SETTINGS_KEY);
		return restoreRenderSettings(raw ? JSON.parse(raw) : null);
	} catch {
		return restoreRenderSettings(null);
	}
}

export function GraphView() {
	const containerRef = useRef<HTMLDivElement | null>(null);
	const canvasRef = useRef<HTMLCanvasElement | null>(null);
	const cardRef = useRef<HTMLDivElement | null>(null);
	const searchInputRef = useRef<HTMLInputElement>(null);
	const rendererRef = useRef<GraphCanvasRenderer | null>(null);
	const posRef = useRef<Map<string, { x: number; y: number }> | null>(null);

	const [snapshot, setSnapshot] = useState<GraphSnapshot | null>(null);
	const snapshotRef = useRef<GraphSnapshot | null>(null);
	snapshotRef.current = snapshot;
	const [loadError, setLoadError] = useState<string>();
	const [selected, setSelected] = useState<Selection | null>(null);
	const [query, setQuery] = useState("");
	const [chainsOpen, setChainsOpen] = useState(false);
	const [settings, setSettings] = useState<GraphRenderSettings>(loadSettings);
	const [sources, setSources] = useState<GraphSource[]>([]);
	const [mapJobs, setMapJobs] = useState<Record<string, JobRecord>>({});
	const [extractError, setExtractError] = useState<string>();
	const [extracting, setExtracting] = useState(false);
	const [practiceError, setPracticeError] = useState<string>();

	const changeSettings: Change = (update) => setSettings((cur) => update(cur));
	useEffect(() => {
		try {
			localStorage.setItem(SETTINGS_KEY, JSON.stringify(sparseRenderSettings(settings)));
		} catch {
			/* best effort */
		}
	}, [settings]);

	// ── data + live refresh ──
	const [refreshTick, setRefreshTick] = useState(0);
	useEffect(() => {
		let cancelled = false;
		mapApi
			.graph()
			.then((d) => !cancelled && (setSnapshot(d), setLoadError(undefined)))
			.catch((e) => !cancelled && setLoadError(String(e.message ?? e)));
		mapApi.sources().then((s) => !cancelled && setSources(s)).catch(() => {});
		return () => {
			cancelled = true;
		};
	}, [refreshTick]);
	useEffect(() => {
		mapApi
			.jobs()
			.then((list) => setMapJobs(Object.fromEntries(list.filter((j) => j.kind === "map" && j.status === "running").map((j) => [j.id, j]))))
			.catch(() => {});
		return subscribeEvents((e: GlobalEvent) => {
			if (e.type === "graph") setRefreshTick((t) => t + 1);
			else if (e.type === "job" && e.job.kind === "map") {
				const job = e.job;
				setMapJobs((cur) => {
					const next = { ...cur };
					if (job.status === "running") next[job.id] = job;
					else delete next[job.id];
					return next;
				});
				if (job.status !== "running") setRefreshTick((t) => t + 1); // coverage changed
			}
		});
	}, []);

	const pending = sources.filter(needsMapping).length;
	const runningJobs = Object.values(mapJobs);
	const update = async () => {
		setExtracting(true);
		setExtractError(undefined);
		try {
			const { jobs } = await mapApi.extract();
			setMapJobs((cur) => ({ ...cur, ...Object.fromEntries(jobs.filter((j) => j.status === "running").map((j) => [j.id, j])) }));
		} catch (e) {
			setExtractError((e as Error).message);
		} finally {
			setExtracting(false);
			setRefreshTick((t) => t + 1);
		}
	};

	// ── search ──
	const visibleIds = useMemo(() => {
		const set = new Set<string>();
		if (snapshot) for (const node of snapshot.nodes) if (isNodeVisible(node, query)) set.add(node.id);
		return set;
	}, [snapshot, query]);

	// ── context menu + delete ──
	const [ctxNode, setCtxNode] = useState<{ id: string; name: string; x: number; y: number } | null>(null);
	const [deletingNode, setDeletingNode] = useState<{ id: string; name: string } | null>(null);
	useEffect(() => {
		if (ctxNode === null) return;
		const onPointerDown = (e: PointerEvent) => {
			if (e.target instanceof Element && e.target.closest("[data-map-menu]")) return;
			setCtxNode(null);
		};
		const onKeyDown = (e: KeyboardEvent) => e.key === "Escape" && setCtxNode(null);
		document.addEventListener("pointerdown", onPointerDown);
		document.addEventListener("keydown", onKeyDown);
		return () => {
			document.removeEventListener("pointerdown", onPointerDown);
			document.removeEventListener("keydown", onKeyDown);
		};
	}, [ctxNode]);

	// ── layout: the positions ledger is the layout; one deterministic d3-force pass only when it is absent ──
	useEffect(() => {
		if (!snapshot || snapshot.nodes.length === 0) return;
		const ledger = snapshot.positions;
		if (ledger) {
			const positions = new Map<string, { x: number; y: number }>();
			for (const node of snapshot.nodes) {
				const p = ledger[node.id];
				if (p) positions.set(node.id, { x: p.x, y: p.y });
			}
			if (positions.size > 0) {
				posRef.current = seedMissingNodePositions(snapshot.nodes, positions);
				return () => {
					posRef.current = null;
				};
			}
		}
		// No ledger (the server lays out on GET, so only a failed write lands here): the same layered layout.
		const birth = new Map(snapshot.propositions.map((n) => [n.id, birthSessionOf(n) ?? ""]));
		posRef.current = layeredLayout(snapshot.nodes, snapshot.logicalEdges, snapshot.impLinks, (id) => birth.get(id) ?? "");
		return () => {
			posRef.current = null;
		};
	}, [snapshot]);

	// ── renderer lifecycle ──
	const isEmpty = snapshot === null || snapshot.nodes.length === 0;
	useEffect(() => {
		if (isEmpty) return;
		const canvas = canvasRef.current;
		if (!canvas) return;
		let initialCamera: { k: number; x: number; y: number } | null = null;
		try {
			const parsed = JSON.parse(localStorage.getItem(CAMERA_KEY) ?? "null") as { k?: unknown; x?: unknown; y?: unknown } | null;
			if (parsed && typeof parsed.k === "number" && parsed.k > 0 && typeof parsed.x === "number" && typeof parsed.y === "number")
				initialCamera = { k: parsed.k, x: parsed.x, y: parsed.y };
		} catch {
			/* corrupt: the renderer fits */
		}
		let cameraTimer: ReturnType<typeof setTimeout> | null = null;
		let lastCamera: { k: number; x: number; y: number } | null = null;
		const save = (t: { k: number; x: number; y: number }) => {
			try {
				localStorage.setItem(CAMERA_KEY, JSON.stringify(t));
			} catch {
				/* best effort */
			}
		};
		const renderer = new GraphCanvasRenderer(
			canvas,
			{
				onSelect: (nodeId, pos) => {
					const snap = snapshotRef.current;
					const node = snap?.nodes.find((n) => n.id === nodeId);
					if (!snap || !node) return;
					setSelected({ node, sourceIds: snap.nodeSessions[node.id] ?? [], anchor: pos });
				},
				onStageClick: () => setSelected(null),
				onContext: (nodeId, pos) => {
					if (nodeId === null) return setCtxNode(null);
					const node = snapshotRef.current?.nodes.find((n) => n.id === nodeId);
					if (node) setCtxNode({ id: node.id, name: node.name, x: pos.x, y: pos.y });
				},
				onCameraMove: (t) => {
					setSelected(null);
					setCtxNode(null);
					lastCamera = t;
					if (cameraTimer) clearTimeout(cameraTimer);
					cameraTimer = setTimeout(() => {
						cameraTimer = null;
						save(t);
					}, 250);
				},
				onCommit: (positions) => {
					void mapApi.positions(positions).catch((err) => console.error("[map] positions commit failed", err));
				},
			},
			initialCamera,
		);
		rendererRef.current = renderer;
		return () => {
			rendererRef.current = null;
			if (cameraTimer && lastCamera) {
				clearTimeout(cameraTimer);
				save(lastCamera);
			}
			renderer.destroy();
		};
	}, [isEmpty]);

	// Feed 1: data (and colours: the theme is re-read on change)
	const appearance = useAppearance();
	useEffect(() => {
		const renderer = rendererRef.current;
		const positions = posRef.current;
		if (!renderer || !snapshot || snapshot.nodes.length === 0 || !positions) return;
		renderer.setData({
			nodes: snapshot.nodes.map((n) => ({ id: n.id, x: positions.get(n.id)?.x ?? 0, y: positions.get(n.id)?.y ?? 0, label: n.name, status: n.status })),
			edges: snapshot.edges.map((e) => ({ from: e.from, to: e.to })),
			logicalEdges: snapshot.logicalEdges.map((e) => ({ from: e.from, to: e.to })),
			impLinks: snapshot.impLinks,
			clusters: snapshot.clusters.map((c) => ({ id: c.id, label: c.label, members: c.members })),
			courses: snapshot.courses,
			keyNodes: keyConcepts(snapshot),
			colors: {
				node: cssVar("--primary", "#e6e6e6"),
				edge: cssVar("--muted-foreground", "#8d8d8d"),
				label: cssVar("--muted-foreground", "#8d8d8d"),
				hoverText: cssVar("--heading", "#ffffff"),
				key: cssVar("--brand", "#e0b765"),
				warn: cssVar("--warn", "#e0b765"),
				bad: cssVar("--bad", "#e5806f"),
				theme: appearance.mode,
			},
			fontFamily: getComputedStyle(document.body).fontFamily,
		});
	}, [snapshot, isEmpty, appearance]);
	// Feed 2: settings (redraw only)
	const deferredSettings = useDeferredValue(settings);
	useEffect(() => {
		rendererRef.current?.setSettings(deferredSettings);
	}, [deferredSettings, isEmpty]);
	// Feed 3: visibility
	useEffect(() => {
		rendererRef.current?.setVisible(query.trim() ? visibleIds : null);
		setSelected((sel) => (sel && visibleIds.has(sel.node.id) ? sel : null));
	}, [visibleIds, query]);
	// Region tuning. Physics tuning is not fed: the renderer defaults equal the server settle, and a feed would reheat the sim on mount.
	useEffect(() => {
		rendererRef.current?.setRegionTuning(settings.regions);
	}, [settings.regions, isEmpty]);

	// Detail card: below the node, flipped above near the bottom edge, clamped horizontally.
	useLayoutEffect(() => {
		const card = cardRef.current;
		const container = containerRef.current;
		if (!selected || !card || !container) return;
		const width = card.offsetWidth;
		const height = card.offsetHeight;
		const margin = 8;
		const gap = 12;
		const left = Math.min(Math.max(selected.anchor.x - width / 2, margin), Math.max(container.clientWidth - width - margin, margin));
		let top = selected.anchor.y + gap;
		if (top + height > container.clientHeight - margin) top = Math.max(selected.anchor.y - gap - height, margin);
		card.style.left = `${left}px`;
		card.style.top = `${top}px`;
		card.style.visibility = "visible";
	}, [selected]);

	// ── practice from the selection ──
	const practice = async (ids: string[]) => {
		setPracticeError(undefined);
		const snap = snapshotRef.current;
		if (!snap || ids.length === 0) return;
		const byId = new Map(snap.propositions.map((n) => [n.id, n]));
		const counts = new Map<string, number>();
		for (const id of ids) {
			const src = byId.get(id)?.provenance[0]?.chatId;
			const course = src ? courseOf(src) : "";
			if (course) counts.set(course, (counts.get(course) ?? 0) + 1);
		}
		const course = [...counts.entries()].sort((a, b) => b[1] - a[1])[0]?.[0];
		if (!course) return setPracticeError("Nessun corso collegato ai nodi selezionati.");
		try {
			const { id } = await mapApi.practice({ kind: "practice", course, practicePropositions: ids });
			location.hash = `#/s/${id}`;
		} catch (e) {
			setPracticeError((e as Error).message);
		}
	};

	const noMatch = snapshot !== null && visibleIds.size === 0 && query.trim() !== "";
	const updateLabel = extracting || runningJobs.length > 0 ? "Aggiorno…" : `Aggiorna mappa (${pending})`;

	return (
		<div className="map-root relative h-full w-full">
			{isEmpty ? (
				<div className="flex h-full w-full items-center justify-center px-4">
					<div className="flex max-w-md flex-col items-center gap-3 text-center text-muted-foreground">
						<WaypointsIcon className="size-10 opacity-40" strokeWidth={1.25} />
						<div className="text-lg font-medium text-foreground">{snapshot === null && !loadError ? "Carico la mappa…" : "La mappa è vuota"}</div>
						{loadError ? (
							<p className="text-sm text-bad">{loadError}</p>
						) : snapshot !== null ? (
							<p className="font-prose text-[15px] leading-relaxed">
								La mappa raccoglie le idee e le catene di ragionamento di ogni lezione che hai fatto. Premi <b className="text-foreground">Aggiorna mappa</b> in alto a
								destra: il sistema legge le lezioni nuove ed estrae le idee, e le vedrai comparire qui.
							</p>
						) : null}
					</div>
				</div>
			) : (
				<div ref={containerRef} className="relative h-full w-full">
					<canvas ref={canvasRef} className="block h-full w-full touch-none" />
					{ctxNode !== null && <div style={{ position: "absolute", left: ctxNode.x, top: ctxNode.y, width: 0, height: 0 }} />}
				</div>
			)}

			{ctxNode !== null && !isEmpty && (
				<div
					data-map-menu
					data-testid="node-context-menu"
					style={{ position: "absolute", left: ctxNode.x, top: ctxNode.y }}
					className="z-30 flex w-fit flex-col gap-0.5 rounded-xl border bg-card p-2 text-xs shadow-xl"
				>
					<button
						type="button"
						data-testid="ctx-node-delete"
						onClick={() => {
							setDeletingNode({ id: ctxNode.id, name: ctxNode.name });
							setCtxNode(null);
						}}
						className="flex items-center rounded-lg px-2.5 py-1.5 text-left text-muted-foreground/80 transition-colors hover:bg-accent hover:text-foreground"
					>
						Elimina
					</button>
				</div>
			)}

			<DeleteNodeDialog
				nodes={deletingNode ? [deletingNode] : null}
				onClose={() => setDeletingNode(null)}
				onDeleted={(ids) => {
					setDeletingNode(null);
					if (selected && ids.includes(selected.node.id)) setSelected(null);
					setRefreshTick((t) => t + 1);
				}}
				onPartial={() => setRefreshTick((t) => t + 1)}
			/>

			{/* reading direction: the layered layout puts foundations at the bottom */}
			{!isEmpty && (
				<div aria-hidden className="pointer-events-none absolute bottom-4 left-3 top-16 z-10 flex flex-col items-center gap-2 text-[10px] text-muted-foreground/70">
					<span className="text-brand">↑</span>
					<span className="w-px flex-1 bg-gradient-to-t from-transparent via-muted-foreground/25 to-brand/60" />
					<span className="[writing-mode:vertical-rl] rotate-180 tracking-wide">fondamenta → concetti derivati</span>
				</div>
			)}

			{/* top-right chrome */}
			<div className="absolute right-3 top-3 z-10 flex max-w-[calc(100%-1.5rem)] flex-col items-end gap-2">
				<div className="relative z-20 flex min-w-0 items-start gap-2">
					{!isEmpty && (
						<div className="graph-chrome-fade-in w-56 min-w-0 shrink rounded-xl border bg-card/85 px-1 py-[3px] shadow-lg backdrop-blur">
							<div className="relative">
								<SearchIcon className="absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
								<input
									ref={searchInputRef}
									value={query}
									onChange={(e) => setQuery(e.target.value)}
									placeholder="Cerca nodi…"
									aria-label="Cerca nodi"
									spellCheck={false}
									className="h-7 w-full rounded-lg border bg-background pl-8 text-xs outline-none placeholder:text-muted-foreground focus:border-muted-foreground"
								/>
							</div>
							{noMatch && <div className="px-1.5 pt-1 text-[11px] text-muted-foreground">Nessun nodo corrisponde.</div>}
						</div>
					)}
					{!isEmpty && (
						<div className="graph-chrome-fade-in shrink-0 rounded-xl border bg-card/85 px-1 py-[3px] shadow-lg backdrop-blur">
							<button
								type="button"
								title="Tutte le catene di ragionamento"
								onClick={() => setChainsOpen(true)}
								data-testid="open-chain-table"
								className="flex h-7 items-center gap-1.5 rounded-lg px-2.5 text-xs text-muted-foreground transition-colors hover:bg-accent"
							>
								<ListTreeIcon className="size-3" />
								Catene
							</button>
						</div>
					)}
					<div className="shrink-0 rounded-xl border bg-card/85 px-1 py-[3px] shadow-lg backdrop-blur">
						<button
							type="button"
							title="Estrae le idee dalle lezioni nuove o cambiate"
							disabled={pending === 0 || extracting || runningJobs.length > 0}
							onClick={() => void update()}
							data-testid="update-map"
							className="flex h-7 items-center gap-1.5 rounded-lg px-2.5 text-xs text-muted-foreground transition-colors hover:bg-accent disabled:opacity-50 disabled:hover:bg-transparent"
						>
							<RefreshCwIcon className={`size-3 ${extracting || runningJobs.length > 0 ? "animate-spin" : ""}`} />
							{updateLabel}
						</button>
					</div>
					{!isEmpty && (
						<div className="graph-chrome-fade-in shrink-0">
							<GraphSettingsControl settings={settings} change={changeSettings} />
						</div>
					)}
				</div>
				{extractError && <div className="rounded-xl border border-bad/50 bg-card/90 px-3 py-1.5 text-xs text-bad shadow-lg">{extractError}</div>}
				{practiceError && <div className="rounded-xl border border-bad/50 bg-card/90 px-3 py-1.5 text-xs text-bad shadow-lg">{practiceError}</div>}
				{runningJobs.map((job) => (
					<div key={job.id} className="flex items-center gap-2 rounded-full border bg-card/85 px-3 py-1.5 text-xs text-muted-foreground shadow-lg backdrop-blur">
						<Loader2Icon className="size-3.5 shrink-0 animate-spin" />
						<span className="max-w-56 truncate text-foreground">{job.title}</span>
						{job.activity && <span className="max-w-40 truncate">{job.activity}</span>}
					</div>
				))}
			</div>

			{selected && !isEmpty && (
				<div
					ref={cardRef}
					style={{ visibility: "hidden", left: 0, top: 0 }}
					className="absolute z-10 w-80 max-w-[calc(100%-1rem)] rounded-xl border bg-card p-4 text-left shadow-lg"
				>
					<div className="text-sm font-medium leading-snug">{selected.node.name}</div>
					<div className="mt-1 font-prose text-[14px] leading-relaxed text-muted-foreground">{selected.node.statement}</div>
					{selected.sourceIds.length > 0 ? (
						<div className="mt-3 flex flex-col gap-1">
							<div className="text-xs text-muted-foreground">Lezioni</div>
							{selected.sourceIds.map((sourceId) => {
								const point = selected.node.provenance.find((p) => p.chatId === sourceId && p.messageId);
								return (
									<a
										key={sourceId}
										href={lessonUrl(sourceId, point?.messageId)}
										className="min-w-0 truncate rounded-md px-2 py-1.5 text-sm text-foreground transition-colors hover:bg-accent"
									>
										{snapshot?.sessions[sourceId] ?? sourceId}
									</a>
								);
							})}
						</div>
					) : (
						<div className="mt-3 text-xs text-muted-foreground">Nessuna lezione registrata.</div>
					)}
				</div>
			)}

			{/* last child: it must paint after everything it covers */}
			<ChainTableOverlay
				open={chainsOpen && !isEmpty}
				nodes={snapshot?.propositions ?? []}
				chains={snapshot?.chains ?? []}
				sessions={snapshot?.sessions ?? {}}
				onClose={() => setChainsOpen(false)}
				onPractice={(ids) => {
					setChainsOpen(false);
					void practice(ids);
				}}
			/>
		</div>
	);
}
