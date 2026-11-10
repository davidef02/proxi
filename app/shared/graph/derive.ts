// Adapted from LearnHarness (github.com/amosblomqvist/learnharness), AGPL-3.0-only.
// Pure derivation: ledger lines -> merged graph -> derived view -> snapshot. Browser-safe (no node imports).
import { formatChainLine } from "./chains.ts";
import { inferenceDepths } from "./layout.ts";
import { attachImpToCommunities, birthSessionOf, birthSessionPartition } from "./session-clusters.ts";
import {
	MIN_CLUSTER_SIZE,
	type Cluster,
	type GraphChain,
	type GraphEdge,
	type GraphNode,
	type GraphSnapshot,
	type Position,
	type ProvenancePoint,
} from "./contracts.ts";

/** A chain's merge key: sorted premises + conclusion. */
export function chainKey(premises: ReadonlyArray<string>, conclusion: string): string {
	return `${[...premises].sort().join("+")}>${conclusion}`;
}

export const samePoint = (a: ProvenancePoint, b: ProvenancePoint): boolean =>
	a.chatId === b.chatId && a.messageId === b.messageId;

/** The deterministic chain-simplification pass (owner, 2026-09-30
 *  evening): strips redundant derivations — same-conclusion chains whose
 *  premises add nothing a leaner kept chain doesn't already derive. Pure
 *  over the input list; the caller owns the ledger rewrite. Exported for
 *  tests.
 *
 *  Same (premises, conclusion) lines are ONE logical chain (read-time
 *  merge semantics) — a key is kept or dropped as a whole, every
 *  provenance line with it. */
export function simplifyChains(lines: GraphChain[]): GraphChain[] {
	if (lines.length < 2) return lines;

	// Group lines by chain key (logical chains).
	const groups = new Map<string, GraphChain[]>();
	for (const line of lines) {
		const key = chainKey(line.premises, line.conclusion);
		const bucket = groups.get(key);
		if (bucket === undefined) groups.set(key, [line]);
		else bucket.push(line);
	}

	// Ancestry over the FULL chain set: every node id reachable as a
	// premise behind an id (A⟹X makes A an ancestor of X; [X,C]⟹Y makes
	// both A and C ancestors of Y). Memoized walk, cycle-guarded (a cycle
	// cannot be minted by the extractor — a premise equal to the
	// conclusion is refused — but the walk stays honest if one ever lands).
	const premisesOf = new Map<string, string[][]>();
	for (const line of lines) {
		const bucket = premisesOf.get(line.conclusion);
		if (bucket === undefined) premisesOf.set(line.conclusion, [line.premises]);
		else bucket.push(line.premises);
	}
	const ancestryMemo = new Map<string, Set<string>>();
	const ancestry = (id: string, visiting: Set<string>): Set<string> => {
		const memo = ancestryMemo.get(id);
		if (memo !== undefined) return memo;
		const out = new Set<string>();
		if (visiting.has(id)) return out;
		visiting.add(id);
		for (const premises of premisesOf.get(id) ?? []) {
			for (const premise of premises) {
				out.add(premise);
				for (const ancestor of ancestry(premise, visiting)) out.add(ancestor);
			}
		}
		visiting.delete(id);
		ancestryMemo.set(id, out);
		return out;
	};

	// Per conclusion: greedy in (premiseCount, createdAt, key).
	const byConclusion = new Map<
		string,
		Array<{ key: string; premises: string[]; createdAt: string }>
	>();
	for (const [key, bucket] of groups) {
		const head = bucket[0]!; // the representative — its premises stand
		const entry = { key, premises: head.premises, createdAt: head.createdAt };
		const list = byConclusion.get(head.conclusion);
		if (list === undefined) byConclusion.set(head.conclusion, [entry]);
		else list.push(entry);
	}
	const droppedKeys = new Set<string>();
	for (const candidates of byConclusion.values()) {
		candidates.sort(
				(a, b) =>
					a.premises.length - b.premises.length ||
					a.createdAt.localeCompare(b.createdAt) ||
					a.key.localeCompare(b.key),
		);
		const kept: typeof candidates = [];
		for (const candidate of candidates) {
			const candidateSet = new Set(candidate.premises);
			const redundant = kept.some((k) => {
					// Rule 1 — prefer-fewest-premises: strict subset (equal keys are
					// grouped, so a kept ⊆ candidate with fewer premises is strict).
					if (k.premises.every((p) => candidateSet.has(p))) return true;
					// Rule 2 — ancestry-cover: every candidate premise is a kept
					// premise, or reachable behind one.
					const covered = new Set(k.premises);
					for (const p of k.premises) {
						for (const ancestor of ancestry(p, new Set())) covered.add(ancestor);
					}
					return candidate.premises.every((p) => covered.has(p));
			});
			if (redundant) droppedKeys.add(candidate.key);
			else kept.push(candidate);
		}
	}
	if (droppedKeys.size === 0) return lines;
	// Never strip the only chain a premise sits in: the map holds no isolated idea (ia-learner rule). A redundant
	// chain whose premise appears nowhere else stays; checked in key order so the result is deterministic.
	const keptIds = new Set<string>();
	for (const line of lines) {
		if (droppedKeys.has(chainKey(line.premises, line.conclusion))) continue;
		for (const id of [...line.premises, line.conclusion]) keptIds.add(id);
	}
	for (const key of [...droppedKeys].sort()) {
		const line = groups.get(key)![0]!;
		if (line.premises.every((p) => keptIds.has(p))) continue;
		droppedKeys.delete(key);
		for (const id of line.premises) keptIds.add(id);
	}
	return lines.filter((line) => !droppedKeys.has(chainKey(line.premises, line.conclusion)));
}

// ─── derivation (reads only — everything below is recomputable) ─────────────

/** The merged STORED truth: propositions + chains (chains-primary, owner
 *  2026-09-30). `nodes` are PROPOSITIONS ONLY — no derived implication
 *  nodes; the auditor's and prober's views see the world, not the
 *  rendering. The canvas-facing VIEW (implication nodes + untyped
 *  arrows) is deriveView() below — the lossy image. */
export interface DerivedGraph {
	nodes: GraphNode[];
	chains: GraphChain[];
}

/**
 * The whole graph from the ledgers. Node LINES may repeat an id (link-
 * appends): the FIRST line defines name/statement/createdAt; later
 * lines' statements and aliases join `aliases`, provenance unions — the
 * name never changes after creation. Chain LINES are assertions, merged
 * by (premises, conclusion) with unioned provenance; the first line's
 * arg_ id / premises / createdAt are the chain's identity.
 * `store events, derive state` — this is the only reader that matters.
 */
export function getGraph(graphStore: { readNodes(): GraphNode[]; readChains(): GraphChain[] }): DerivedGraph {
	// Merge node lines by id (first line defines; later lines add phrasings).
	const byId = new Map<string, GraphNode>();
	for (const line of graphStore.readNodes()) {
		const existing = byId.get(line.id);
		if (!existing) {
			byId.set(line.id, { ...line, aliases: [...line.aliases], provenance: [...line.provenance] });
			continue;
		}
		// A later line's statement is a phrasing, not a replacement.
		for (const phrasing of [line.statement, ...line.aliases]) {
			if (phrasing.length > 0 && !existing.aliases.includes(phrasing)) existing.aliases.push(phrasing);
		}
		for (const pt of line.provenance) {
			if (!existing.provenance.some((q) => samePoint(q, pt))) existing.provenance.push(pt);
		}
	}

	// Merge chain assertion lines by (premises, conclusion): the first
	// line's id/premises/createdAt stand, provenance unions.
	const chains: GraphChain[] = [];
	const byKey = new Map<string, GraphChain>();
	for (const line of graphStore.readChains()) {
		const key = chainKey(line.premises, line.conclusion);
		const existing = byKey.get(key);
		if (existing) {
			for (const pt of line.provenance) {
				if (!existing.provenance.some((q) => samePoint(q, pt))) existing.provenance.push(pt);
			}
		} else {
			const chain: GraphChain = { ...line, premises: [...line.premises], provenance: [...line.provenance] };
			byKey.set(key, chain);
			chains.push(chain);
		}
	}
	return { nodes: [...byId.values()], chains };
}

/** The DERIVED view — what the canvas renders. A SINGLE-premise chain
 *  collapses to ONE direct arrow premise → conclusion (no imp node:
 *  a chain is always N-in/1-out, so one premise is the degenerate
 *  1-in-1-out case). A MULTI-premise chain mints an implication NODE
 *  (id = the chain's arg_ id; name "⟹"; statement = the chain line;
 *  the chain's provenance/createdAt) plus arrows: each premise → the
 *  implication node, the implication node → the conclusion — the
 *  canvas renders imp nodes as pure geometry (owner, rendering arc:
 *  invisible, unpickable; co-premise lines converge headless to a
 *  point, one arrowed line leaves it). Physics keeps imp nodes as
 *  regular citizens (half link distance — layout.ts). Dangling chains
 *  (a member node missing) drop here — the deletion cascades should
 *  prevent them; this is the honest belt. */
export function deriveView(graph: DerivedGraph): {
	nodes: GraphNode[];
	edges: GraphEdge[];
	logicalEdges: GraphEdge[];
	impLinks: Array<{ id: string; premises: string[]; conclusion: string }>;
} {
	const names: Record<string, { name: string }> = {};
	for (const node of graph.nodes) names[node.id] = { name: node.name };
	const live = new Set(graph.nodes.map((node) => node.id));
	const impNodes: GraphNode[] = [];
	const edges: GraphEdge[] = [];
	// The LOGICAL edge set (owner, 2026-09-30): every chain contracted to
	// direct premise→conclusion arrows over PROPOSITION ids only —
	// what inference semantics (the depth pass) consumes. The drawn edges
	// are the canvas image; these are the meaning.
	const impLinks: Array<{ id: string; premises: string[]; conclusion: string }> = [];
	const logicalEdges: GraphEdge[] = [];
	const logicalSeen = new Set<string>();
	const emitLogical = (from: string, to: string): void => {
		const key = from + "\u0000" + to;
		if (logicalSeen.has(key)) return;
		logicalSeen.add(key);
		logicalEdges.push({ from, to });
	};
	for (const chain of graph.chains) {
		if (!live.has(chain.conclusion) || !chain.premises.every((premise) => live.has(premise))) continue;
		for (const premise of chain.premises) emitLogical(premise, chain.conclusion);
		// Single premise: the argument IS a plain arrow — no geometry
		// point needed (the 1-in/1-out collapse; rendering arc, owner).
		if (chain.premises.length === 1) {
			edges.push({ from: chain.premises[0]!, to: chain.conclusion });
			continue;
		}
		impNodes.push({
			id: chain.id,
			name: "⟹",
			statement: formatChainLine(chain, names),
			aliases: [],
			createdAt: chain.createdAt,
			provenance: [...chain.provenance],
		});
		// The junction's linkage — what the depth pass needs for the
		// midpoint anchor (owner, 2026-09-30).
		impLinks.push({ id: chain.id, premises: [...chain.premises], conclusion: chain.conclusion });
		for (const premise of chain.premises) edges.push({ from: premise, to: chain.id });
		edges.push({ from: chain.id, to: chain.conclusion });
	}
	return { nodes: [...graph.nodes, ...impNodes], edges, logicalEdges, impLinks };
}


/** Each course's main concepts, the ones the zoomed-out map labels: first the tops of its chains (conclusions nothing
 *  else IN THE SAME COURSE builds on, so a later course reusing them does not demote them), highest first, then the
 *  most derived of the rest; about one in ten of its propositions, at least three (fewer only in a smaller course). */
/** Transitive fan-in (ancestor closure size): how many premises a proposition rests on transitively in the DAG. Cycle-safe. */
export function computeTransitiveFanIn(
	nodes: ReadonlyArray<Pick<GraphNode, "id">>,
	logicalEdges: ReadonlyArray<GraphEdge>,
): Map<string, number> {
	const valid = new Set(nodes.map((n) => n.id));
	const incoming = new Map<string, string[]>();
	for (const e of logicalEdges) {
		if (e.from === e.to || !valid.has(e.from) || !valid.has(e.to)) continue;
		let list = incoming.get(e.to);
		if (!list) incoming.set(e.to, (list = []));
		list.push(e.from);
	}
	const memo = new Map<string, Set<string>>();
	const visiting = new Set<string>();

	function getAncestors(id: string): Set<string> {
		const cached = memo.get(id);
		if (cached) return cached;
		if (visiting.has(id)) return new Set();
		visiting.add(id);
		const ancestors = new Set<string>();
		for (const parent of incoming.get(id) ?? []) {
			ancestors.add(parent);
			for (const anc of getAncestors(parent)) ancestors.add(anc);
		}
		visiting.delete(id);
		memo.set(id, ancestors);
		return ancestors;
	}

	const fanIn = new Map<string, number>();
	for (const n of nodes) fanIn.set(n.id, getAncestors(n.id).size);
	return fanIn;
}

/** Each course's main concepts, the ones the zoomed-out map labels: selected per lesson so every lesson column
 *  has its landmarks (1-2 chain tops/capstones per lesson), ranked by local chain top status, transitive fan-in
 *  (how many propositions it synthesizes), cross-lesson downstream reuse, and inference depth, with a course floor
 *  (at least 3 when available). */
export function keyConcepts(snap: Pick<GraphSnapshot, "propositions" | "logicalEdges" | "courses">): Set<string> {
	const depth = inferenceDepths(snap.propositions, snap.logicalEdges);
	const fanIn = computeTransitiveFanIn(snap.propositions, snap.logicalEdges);
	const propMap = new Map(snap.propositions.map((n) => [n.id, n]));
	const props = new Set(propMap.keys());

	const birthMap = new Map<string, string | null>();
	for (const p of snap.propositions) birthMap.set(p.id, birthSessionOf(p));

	const crossOut = new Map<string, number>();
	for (const e of snap.logicalEdges) {
		if (!props.has(e.from) || !props.has(e.to)) continue;
		const fromSession = birthMap.get(e.from);
		const toSession = birthMap.get(e.to);
		if (fromSession && toSession && fromSession !== toSession) {
			crossOut.set(e.from, (crossOut.get(e.from) ?? 0) + 1);
		}
	}

	const out = new Set<string>();
	for (const c of snap.courses) {
		const mine = new Set(c.members.filter((id) => props.has(id)));
		const inCourse = snap.logicalEdges.filter((e) => mine.has(e.from) && mine.has(e.to));
		const coursePremise = new Set(inCourse.map((e) => e.from));
		const courseConclusion = new Set(inCourse.map((e) => e.to));
		const courseTop = (id: string) => (courseConclusion.has(id) && !coursePremise.has(id) ? 1 : 0);

		const byLesson = new Map<string, string[]>();
		for (const id of mine) {
			const s = birthMap.get(id) ?? "__unknown__";
			let list = byLesson.get(s);
			if (!list) byLesson.set(s, (list = []));
			list.push(id);
		}

		const compareProps = (a: string, b: string, localTop?: (id: string) => number) => {
			if (localTop) {
				const topDiff = localTop(b) - localTop(a);
				if (topDiff !== 0) return topDiff;
			}
			const topCourseDiff = courseTop(b) - courseTop(a);
			if (topCourseDiff !== 0) return topCourseDiff;

			const fanInDiff = (fanIn.get(b) ?? 0) - (fanIn.get(a) ?? 0);
			if (fanInDiff !== 0) return fanInDiff;

			const crossDiff = (crossOut.get(b) ?? 0) - (crossOut.get(a) ?? 0);
			if (crossDiff !== 0) return crossDiff;

			const depthDiff = (depth.get(b) ?? 0) - (depth.get(a) ?? 0);
			if (depthDiff !== 0) return depthDiff;

			return a < b ? -1 : 1;
		};

		const courseRanked = [...mine].sort((a, b) => compareProps(a, b));

		for (const [_lessonId, members] of byLesson) {
			const inLesson = snap.logicalEdges.filter((e) => members.includes(e.from) && members.includes(e.to));
			const lessonPremise = new Set(inLesson.map((e) => e.from));
			const lessonConclusion = new Set(inLesson.map((e) => e.to));
			const lessonTop = (id: string) => (lessonConclusion.has(id) && !lessonPremise.has(id) ? 1 : 0);

			const rankedInLesson = [...members].sort((a, b) => compareProps(a, b, lessonTop));
			const take = members.length >= 3 ? 2 : 1;
			for (const id of rankedInLesson.slice(0, take)) {
				out.add(id);
			}
		}

		const targetCourseCount = Math.min(mine.size, Math.max(3, Math.ceil(mine.size / 10)));
		for (const id of courseRanked) {
			const courseSelectedCount = [...mine].filter((m) => out.has(m)).length;
			if (courseSelectedCount >= targetCourseCount) break;
			out.add(id);
		}
	}
	return out;
}

// ─── the /api/graph payload ──────────────────────────────────────────────────

/** Snapshot over raw ledger lines. `sessions` = source id -> display label (unlabeled sources fall back to the id). */
export function buildSnapshot(
	nodeLines: GraphNode[],
	chainLines: GraphChain[],
	sessions: Record<string, string>,
	positions: Record<string, Position> = {},
	/** course dir -> course title (the frame label); unknown courses show their dir name. */
	courseLabels: Record<string, string> = {},
): GraphSnapshot {
	const graph = getGraph({ readNodes: () => nodeLines, readChains: () => chainLines });
	const view = deriveView(graph);
	const nodeChats = new Map<string, Set<string>>();
	for (const node of view.nodes) {
		for (const pt of node.provenance) {
			let s = nodeChats.get(node.id);
			if (!s) nodeChats.set(node.id, (s = new Set()));
			s.add(pt.chatId);
		}
	}
	const nodeSessions: Record<string, string[]> = {};
	for (const [id, set] of nodeChats) nodeSessions[id] = [...set];
	const clusters: Cluster[] = birthSessionPartition(graph.nodes)
		.filter((c) => c.members.length >= MIN_CLUSTER_SIZE)
		.map((c) => ({ id: c.id, label: sessions[c.id] ?? null, members: c.members }));
	attachImpToCommunities(clusters, graph.nodes, graph.chains);
	// Courses: every birth lesson (any size) grouped by its course dir, so no node falls outside its course frame.
	const lessons = birthSessionPartition(graph.nodes).map((c) => ({ id: c.id, members: [...c.members] }));
	attachImpToCommunities(lessons, graph.nodes, graph.chains);
	const byCourse = new Map<string, string[]>();
	for (const l of lessons) {
		const course = l.id.split("/")[0]!;
		byCourse.set(course, [...(byCourse.get(course) ?? []), ...l.members]);
	}
	const courses: Cluster[] = [...byCourse.entries()]
		.sort(([a], [b]) => (a < b ? -1 : 1))
		.map(([id, members]) => ({ id, label: courseLabels[id] ?? id, members: members.sort() }));
	return {
		nodes: view.nodes,
		propositions: graph.nodes,
		edges: view.edges,
		logicalEdges: view.logicalEdges,
		impLinks: view.impLinks,
		chains: graph.chains,
		sessions,
		nodeSessions,
		clusters,
		courses,
		...(Object.keys(positions).length > 0 ? { positions } : {}),
	};
}
