// Seeds a repo with 3 lesson stubs + a realistic small map (Vue reactivity, Italian) through mergeFindings.
// Usage: node --import tsx app/test/seed-graph.ts <repoRoot>   (or import seedGraph in tests)
import * as fs from "node:fs";
import * as path from "node:path";
import type { Findings } from "../server/types.ts";
import { mergeFindings, readSnapshot } from "../server/graph/store.ts";

export const LESSONS = [
  { n: 1, stem: "0001-il-ref-e-una-scatola", title: "Il ref è una scatola che sa chi la legge" },
  { n: 2, stem: "0002-computed-e-watch", title: "Computed e watch: valori derivati ed effetti" },
  { n: 3, stem: "0003-props-ed-eventi", title: "Props ed eventi: il contratto di un componente" },
];

function lessonHtml(n: number, title: string): string {
  const doc = { metadata: { lessonNumber: n, slug: title.toLowerCase().replace(/\W+/g, "-"), title, topic: title }, nodes: [] };
  return `<!doctype html><html><head><title>${title}</title></head><body><h1>${title}</h1>\n<script type="application/json" id="lesson-data">${JSON.stringify(doc)}</script></body></html>`;
}

const P = (id: string, name: string, statement: string, nodeId?: string) => ({ id, name, statement, nodeId });

const L1: Findings = {
  propositions: [
    P("p1", "Reattività", "Un framework reattivo ricalcola da solo ciò che dipende da un dato quando il dato cambia.", "n1"),
    P("p2", "ref", "Un ref è un oggetto con una proprietà value: leggerla registra chi legge, scriverla avvisa chi ha letto.", "n1"),
    P("p3", "Tracking", "Durante l'esecuzione di un effetto, ogni lettura di un ref registra quell'effetto come dipendente.", "n2"),
    P("p4", "Trigger", "Scrivere un valore diverso in un ref riesegue gli effetti che lo hanno letto.", "n2"),
    P("p5", "Proxy", "Un oggetto reactive intercetta letture e scritture delle proprietà tramite un Proxy.", "n3"),
    P("p6", "Niente re-render", "Vue aggiorna solo i nodi del DOM che dipendono dal dato cambiato, senza rieseguire l'intero componente.", "n4"),
    P("p7", "Dipendenze esatte", "Le dipendenze di un effetto si determinano a runtime dalle letture effettive, non da una lista dichiarata.", "n3"),
    P("p8", "Destrutturazione", "Destrutturare un oggetto reactive copia i valori e perde il collegamento con le dipendenze.", "n5"),
  ],
  chains: [
    { premises: ["p2", "p3"], conclusion: "p7" },
    { premises: ["p2", "p4"], conclusion: "p1" },
    { premises: ["p7", "p4"], conclusion: "p6" },
    { premises: ["p5"], conclusion: "p3" },
    { premises: ["p5", "p3"], conclusion: "p8" },
  ],
};
const L2: Findings = {
  propositions: [
    P("p1", "computed", "Un computed è un valore derivato che si ricalcola solo quando una delle sue dipendenze cambia.", "n1"),
    P("p2", "Cache del computed", "Un computed memorizza l'ultimo risultato e lo riusa finché le dipendenze non cambiano.", "n1"),
    P("p3", "watch", "Un watch esegue un effetto collaterale quando la sorgente osservata cambia.", "n2"),
    P("p4", "Effetto vs valore", "Un valore derivato va espresso con computed, un effetto collaterale con watch.", "n3"),
    P("p5", "Flush", "Gli effetti di watch vengono raggruppati ed eseguiti prima del rendering successivo.", "n4"),
    P("p6", "Cleanup", "Un watch può registrare una funzione di pulizia che parte prima della riesecuzione successiva.", "n5"),
    P("p7", "Purezza", "Una funzione computed non deve avere effetti collaterali.", "n3"),
    P("p8", "Reattività", "Un framework reattivo ricalcola in automatico ciò che dipende da un dato.", "n1"),
  ],
  chains: [
    { premises: ["p3", "p5"], conclusion: "p6" },
    { premises: ["p1", "p2"], conclusion: "p7" },
    { premises: ["p1", "p3"], conclusion: "p4" },
    { premises: ["p8"], conclusion: "p1" },
  ],
};
const L3: Findings = {
  propositions: [
    P("p1", "Props in sola lettura", "Un componente figlio non deve modificare le proprie props: appartengono al genitore.", "n1"),
    P("p2", "Flusso unidirezionale", "I dati scendono dal genitore al figlio e gli eventi risalgono dal figlio al genitore.", "n1"),
    P("p3", "emit", "Un componente comunica col genitore emettendo eventi con nome e payload.", "n2"),
    P("p4", "v-model", "v-model su un componente è zucchero per una prop modelValue e un evento update:modelValue.", "n3"),
    P("p5", "Contratto", "Props ed eventi dichiarati formano il contratto pubblico di un componente.", "n4"),
    P("p6", "Una sola fonte", "Ogni dato ha un solo proprietario che può modificarlo.", "n1"),
    P("p7", "defineModel", "defineModel dichiara in un colpo solo la prop e l'evento di un v-model.", "n5"),
  ],
  chains: [
    { premises: ["p1", "p3"], conclusion: "p2" },
    { premises: ["p2", "p3"], conclusion: "p4" },
    { premises: ["p6"], conclusion: "p1" },
    { premises: ["p4"], conclusion: "p7" },
    { premises: ["p1", "p2", "p3"], conclusion: "p5" },
  ],
};

export async function seedGraph(repoRoot: string, course = "vue-vetrine"): Promise<void> {
  const dir = path.join(repoRoot, "courses", course, "lessons");
  fs.mkdirSync(dir, { recursive: true });
  for (const l of LESSONS) fs.writeFileSync(path.join(dir, l.stem + ".html"), lessonHtml(l.n, l.title));
  await mergeFindings(repoRoot, `${course}/${LESSONS[0].stem}`, L1);
  // lesson 2's "Reattività" restates lesson 1's idea: linked, not duplicated
  const reatt = (await readSnapshot(repoRoot)).propositions.find((n) => n.name === "Reattività")!;
  const l2: Findings = { ...L2, propositions: L2.propositions.map((p) => (p.id === "p8" ? { ...p, linkTo: reatt.id } : p)) };
  await mergeFindings(repoRoot, `${course}/${LESSONS[1].stem}`, l2);
  await mergeFindings(repoRoot, `${course}/${LESSONS[2].stem}`, L3);
}

if (process.argv[1]?.endsWith("seed-graph.ts") && process.argv[2]) {
  await seedGraph(path.resolve(process.argv[2]));
  console.log("seeded", process.argv[2]);
}
