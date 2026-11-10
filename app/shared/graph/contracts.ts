// Adapted from LearnHarness (github.com/amosblomqvist/learnharness), AGPL-3.0-only.
// Map contracts: propositions (nodes) + reasoning chains. Edges are never stored, they are derived (derive.ts).
import type { MapNodeStatus } from "../protocol.ts";

/** One place an idea came up: chatId = GraphSource.id (`<course>/<lesson file stem>`), messageId = lesson node id ("" = lesson-level). */
export interface ProvenancePoint {
  chatId: string;
  messageId: string;
}

export interface GraphNode {
  /** "n_<random>" (propositions) or "arg_<random>" (derived implication nodes). */
  id: string;
  /** Short glossary-style handle; the canvas label. */
  name: string;
  /** One true, self-contained proposition. */
  statement: string;
  aliases: string[];
  createdAt: string;
  provenance: ProvenancePoint[];
  /** Worst mastery state of its provenance lesson nodes (snapshot only, never stored). */
  status?: MapNodeStatus;
}

/** Premises that JOINTLY entail the conclusion. */
export interface GraphChain {
  id: string; // "arg_<random>"
  premises: string[];
  conclusion: string;
  provenance: ProvenancePoint[];
  createdAt: string;
}

/** Derived untyped arrow. */
export interface GraphEdge {
  from: string;
  to: string;
}

export interface Position {
  x: number;
  y: number;
}

/** A birth-lesson cluster (hull drawn for >= 3 members). */
export interface Cluster {
  id: string;
  label: string | null;
  members: string[];
}

export interface GraphSnapshot {
  /** Propositions + derived implication nodes (what the canvas draws). */
  nodes: GraphNode[];
  /** Stored propositions only. */
  propositions: GraphNode[];
  edges: GraphEdge[];
  logicalEdges: GraphEdge[];
  impLinks: Array<{ id: string; premises: string[]; conclusion: string }>;
  chains: GraphChain[];
  /** source id -> "Lezione N · titolo". */
  sessions: Record<string, string>;
  /** node id -> source ids that touched it. */
  nodeSessions: Record<string, string[]>;
  clusters: Cluster[];
  /** One frame per course (`<course>` prefix of the birth lesson): lesson clusters of the same course, imp nodes included. */
  courses: Cluster[];
  positions?: Record<string, Position>;
}

/** Hull presentation rule: a region needs at least three propositions. */
export const MIN_CLUSTER_SIZE = 3;
