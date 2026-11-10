// Adapted from LearnHarness (github.com/amosblomqvist/learnharness), AGPL-3.0-only.

export interface ReasoningChain {
  id: string;
  premises: string[];
  conclusion: string;
}

/** Stored chains restricted to a selection: a chain rides along iff its conclusion and every premise are selected;
 *  a node in no surviving chain becomes a singleton chain (premises: []). Deterministic (sorted by id). */
export function chainsForSelection(
  nodes: ReadonlyArray<{ id: string }>,
  chains: ReadonlyArray<{ id: string; premises: string[]; conclusion: string }>,
): ReasoningChain[] {
  const selected = new Set(nodes.map((n) => n.id));
  const chained = new Set<string>();
  const included: ReasoningChain[] = [];
  for (const chain of chains) {
    if (!selected.has(chain.conclusion)) continue;
    if (!chain.premises.every((p) => selected.has(p))) continue;
    included.push({ id: chain.id, premises: [...chain.premises].sort(), conclusion: chain.conclusion });
    chained.add(chain.conclusion);
    for (const p of chain.premises) chained.add(p);
  }
  for (const node of nodes) if (!chained.has(node.id)) included.push({ id: node.id, premises: [], conclusion: node.id });
  return included.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
}

/** "Premise one, Premise two → Conclusion" by proposition names. */
export function formatChainLine(
  chain: Pick<ReasoningChain, "premises" | "conclusion">,
  propositions: Readonly<Record<string, { name: string }>>,
): string {
  const nameOf = (id: string) => propositions[id]?.name ?? id;
  const premises = [...chain.premises].sort().map(nameOf).join(", ");
  return premises !== "" ? `${premises} → ${nameOf(chain.conclusion)}` : `${nameOf(chain.conclusion)} (nessuna premessa)`;
}
