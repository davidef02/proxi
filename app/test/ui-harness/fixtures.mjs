// Realistic SessionState snapshots for the harness. Plain JS so the node-side mock can import it.
const at = "2026-10-27T10:00:00.000Z";
const usage = { inputTokens: 5200, outputTokens: 1800, cacheReadTokens: 40000, cacheCreationTokens: 3000, costUsd: 0.37, turns: 4 };

const lesson = `## Il ref è una scatola che sa chi la legge

Quando scrivi \`const n = ref(0)\`, Vue non salva un numero: crea un oggetto con una proprietà \`value\` e, dietro le quinte, una **lista di chi la sta leggendo**. Ogni volta che qualcuno legge \`n.value\` dentro un effetto, finisce in quella lista; quando scrivi, Vue sveglia solo loro.

Per fissare l'idea, immagina che il costo di un aggiornamento sia proporzionale ai lettori $r$ e non alla dimensione $N$ dell'albero:

$$T_{\\text{update}} = O(r) \\quad\\text{invece di}\\quad O(N)$$

In pratica, con $r = 3$ lettori su $N = 400$ componenti, ne tocchi tre.

\`\`\`mermaid
graph TD
  A["ref.value letto"] --> B["effetto registrato"]
  B --> C["ref.value scritto"]
  C --> D["solo i lettori si rieseguono"]
\`\`\`

- Leggi dentro un \`computed\`: il computed diventa lettore.
- Leggi fuori da un effetto: nessuno ti registra.

\`\`\`ts
const n = ref(0);
watchEffect(() => console.log(n.value)); // si registra come lettore
n.value++; // sveglia solo il watchEffect
\`\`\``;

const research = `**Sintesi.** Dalla 3.5 in poi i \`ref\` usano un sistema di versioni e *link* tra dipendenze e subscriber, con meno allocazioni.

**Risultati.**
1. La reattività è passata a un grafo con liste doppiamente collegate.
2. Il consumo di memoria cala di circa il 56%.

**Lacune.** Non ho trovato benchmark indipendenti.`;

const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 520 180" width="520" height="180"><g fill="none" stroke="#e6e6e6" stroke-width="1.6"><rect x="20" y="60" width="120" height="60" rx="8"/><rect x="200" y="20" width="120" height="50" rx="8"/><rect x="200" y="110" width="120" height="50" rx="8"/><rect x="380" y="60" width="120" height="60" rx="8"/><path d="M140 90 L200 45 M140 90 L200 135 M320 45 L380 90 M320 135 L380 90"/></g><g fill="#e6e6e6" font-family="monospace" font-size="14" text-anchor="middle"><text x="80" y="95">ref</text><text x="260" y="50">effetto A</text><text x="260" y="140">effetto B</text><text x="440" y="95">DOM</text></g></svg>`;

const demo = `<!doctype html><html><body style="margin:0;padding:16px;font:15px system-ui;background:#fff;color:#111">
<h3 style="margin:0 0 8px">Quanti lettori svegli?</h3>
<input id="r" type="range" min="1" max="20" value="3" style="width:100%"> <p>Lettori: <b id="v">3</b> su 400 componenti → <b id="p">0.8</b>% toccati</p>
<script>const r=document.getElementById('r');r.oninput=()=>{v.textContent=r.value;p.textContent=(r.value/4).toFixed(1)}</script></body></html>`;

const job = (id, kind, extra) => ({ id, kind, title: "", model: "sonnet", status: "done", activity: "", createdAt: at, updatedAt: at, costUsd: 0.04, ...extra });

const quiz = (id, extra = {}) => ({
  id, at, kind: "quiz", requestId: "rq-" + id, displayOrder: [0, 1, 2],
  input: {
    kind: "check", nodeId: "N1",
    question: "Dopo `n.value++`, quali parti dell'interfaccia vengono rieseguite?",
    options: [
      { label: "Tutti i componenti dell'app, come in React", value: "a" },
      { label: "Solo gli effetti che hanno **letto** `n.value`", value: "b" },
      { label: "Solo il componente che ha dichiarato `n`, e i suoi figli", value: "c" },
    ],
    explanation: "Vue registra chi legge `n.value` e sveglia soltanto quelli: costo $O(r)$.",
    correctAnswer: "b",
    misconceptions: [{ value: "c", misconception: "Questo è il modello di React: re-render dell'albero, non tracciamento dei lettori." }],
  },
  ...extra,
});

const planInput = {
  topic: "Reattività di Vue", title: "Il ref è una scatola che sa chi la legge",
  approach: "Partiamo da una domanda sul tuo modello mentale, poi costruiamo il tracciamento dei lettori nodo per nodo.\n\nOgni nodo si chiude con una verifica.",
  dag: 'graph TD\n  N1["Nodo 1: il ref come scatola"] --> N2["Nodo 2: effetti e lettori"]\n  N1 --> N3["Nodo 3: computed"]\n  N2 --> N4["Nodo 4: flush e batching"]\n  N3 --> N4',
  goal: "Prevedere quali parti di un'interfaccia Vue si aggiornano dopo una scrittura.",
};

const base = (id, title, items, status, extra = {}) => ({
  id, title, createdAt: at, updatedAt: at, status, usage, items,
  config: { kind: "teach", course: "vue-vetrine" }, pendingRequestId: null, plan: null, exercises: [], ...extra,
});

const T = (id, text, streaming = false) => ({ id, at, kind: "teacher", text, streaming });
const plan = { topic: planInput.topic, goal: planInput.goal, dag: planInput.dag, approved: true, nodeStatus: { N1: "done", N2: "current", N3: "todo", N4: "todo" } };

export function scenario(id) {
  switch (id) {
    case "ask0":
      return base(id, "Reattività di Vue", [{ id: "ask1", at, kind: "ask", requestId: "rq-ask1", input: { question: "Vuoi che vengano create e sincronizzate le flashcard Anki per questa sessione? (Sì / No)", options: [{ label: "Sì", value: "si" }, { label: "No", value: "no" }] } }], "waiting", { pendingRequestId: "rq-ask1" });
    case "quiz":
      return base(id, "Reattività di Vue", [T("t1", lesson), { id: "j1", at, kind: "job", job: job("j1", "visual", { title: "Figura: ref e lettori", status: "running", activity: "render SVG" }) }, quiz("q1", {})], "waiting", { pendingRequestId: "rq-q1", plan });
    case "quizlong":
      return base(id, "Reattività di Vue", [T("t1", lesson), quiz("q1", { input: { ...quiz("x").input, kind: "predict", multiSelect: true, question: "Osserva il frammento e rispondi con attenzione: quali tra le seguenti affermazioni sul comportamento del tracciamento delle dipendenze, dopo `n.value++` dentro un `watchEffect` annidato in un `computed`, sono vere?", options: [1, 2, 3, 4, 5].map((k) => ({ label: "Opzione molto lunga numero " + k + ": il sistema reattivo registra il lettore nel momento in cui la proprietà viene letta dentro l'effetto, e non quando l'effetto viene dichiarato, quindi una lettura condizionale cambia l'insieme dei lettori a ogni esecuzione.", value: "o" + k })), correctAnswer: ["o1"], misconceptions: [] } })], "waiting", { pendingRequestId: "rq-q1", plan });
    case "demo":
      return base(id, "Demo", [T("t1", "Guarda la demo."), { id: "j-d", at, kind: "job", job: job("j-d", "demo", { title: "Quanti lettori svegli?", result: { kind: "demo", title: "Quanti lettori svegli?", html: demo, notes: "Muovi il cursore." } }) }], "idle");
    case "plan":
      return base(id, "Reattività di Vue", [T("t1", "Ho pensato a un percorso in quattro nodi. Guardalo e dimmi se va bene."), { id: "p1", at, kind: "plan", requestId: "rq-p1", input: planInput }], "waiting", { pendingRequestId: "rq-p1" });
    case "practice":
      return base(id, "Ripasso", [{ id: "pb1", at, kind: "problem", input: { mode: "discrimination", statement: "Dato `const n = ref(0)`, quali tra questi effetti si riesegue dopo `n.value++`?\n\n> [!TIP]\n> Pensa a chi ha letto.", modelSolution: "x", nodeKeys: [] } }], "idle", {});
    default: // transcript
      return base(id, "Il ref è una scatola che sa chi la legge", [
        T("t1", lesson),
        { id: "tool1", at, kind: "tool", tool: "mcp__app__write_node", input: { planNode: "N1", node: { title: "Il ref come scatola" } }, status: "done", resultText: "Nodo scritto." },
        { id: "node1", at, kind: "node", nodeId: "il-ref", planNode: "N1", title: "Il ref come scatola", lessonPath: "courses/vue-vetrine/lessons/0001-x.html", anki: "2 carte", node: {
          nodeId: "il-ref", title: "Il ref come scatola: $n.value$ sa chi lo legge", tier: "derivation",
          motivation: "<p>Perché un <strong>ref</strong> aggiorna solo ciò che lo ha letto?</p>",
          content: "<h4>La scatola</h4><p>Quando un effetto legge <code>n.value</code>, il ref annota il lettore. Con $k$ lettori il costo di una scrittura è $O(k)$.</p><ul><li>legge, quindi viene annotato</li><li>scrive, quindi sveglia i lettori</li></ul><p class=\"callout\">Il ref non conosce il DOM: conosce i suoi lettori.</p><table><thead><tr><th>Modello</th><th>Chi si aggiorna</th></tr></thead><tbody><tr><td>React</td><td>l'intero componente</td></tr><tr><td>Vue</td><td>solo i lettori</td></tr></tbody></table>",
          math: ["\\sum_{i=1}^{k} c_i = O(k)"],
          codeBlocks: [{ language: "ts", code: "const n = ref(0)\nwatchEffect(() => console.log(n.value))\nn.value++", caption: "Un lettore, una scrittura" }],
          visuals: [{ type: "mermaid", content: 'graph LR\n  A["effetto"] -->|legge| B["ref"]', caption: "Chi legge chi" }],
          connection: "<p>Questo regge il <em>computed</em> del nodo successivo.</p><blockquote>Una citazione.</blockquote>",
          quiz: { question: "Chi viene svegliato?", options: ["tutti", "i lettori"], correctAnswer: "i lettori", correctIndex: 2, explanation: "Solo chi ha letto." },
        } },
        quiz("q1", { result: { selected: ["c"], dontKnow: false, correct: false, confidence: 70, note: "Pensavo che i figli ereditassero l'aggiornamento." } }),
        { id: "l1", at, kind: "learner", text: "Ok, ma allora un `computed` è un lettore anche lui?" },
        T("t2", "Sì: un `computed` legge i suoi ingredienti e **a sua volta** può essere letto. È una catena di lettori."),
        { id: "tool2", at, kind: "tool", tool: "WebSearch", input: { query: "vue 3.5 reactivity refactor" }, status: "running" },
        { id: "j-r", at, kind: "job", job: job("j-r", "research", { title: "Ricerca: reattività 3.5", result: { kind: "research", brief: research, sources: [{ title: "Vue 3.5 release notes", url: "https://blog.vuejs.org/posts/vue-3-5" }, { title: "Reactivity in depth", url: "https://vuejs.org/guide/extras/reactivity-in-depth" }] } }) },
        { id: "j-v", at, kind: "job", job: job("j-v", "visual", { title: "Figura: ref e lettori", result: { kind: "visual", svg, caption: "Un ref con due effetti che lo leggono e il DOM che ne dipende." } }) },
        { id: "j-d", at, kind: "job", job: job("j-d", "demo", { title: "Quanti lettori svegli?", result: { kind: "demo", title: "Quanti lettori svegli?", html: demo, notes: "Muovi il cursore: il costo segue i lettori, non l'albero." } }) },
        { id: "j-m", at, kind: "job", job: job("j-m", "map", { title: "Mappa: 0001 il ref", result: { kind: "map", source: "vue-vetrine/0001", added: 4, linked: 2, chains: 1 } }) },
        { id: "j-e", at, kind: "job", job: job("j-e", "research", { title: "Ricerca: benchmark", status: "error", error: "il subagente ha terminato senza consegnare" }) },
        { id: "ex1", at, kind: "exercise", exercise: { course: "vue-vetrine", name: "06-v-model", dir: "courses/vue-vetrine/exercises/06-v-model", files: ["Esercizio.vue", "esercizio.test.ts"], instructions: "Completa `Esercizio.vue` perché il test passi.", lastRun: { ok: false, passed: 2, failed: 1, total: 3, failures: [{ name: "emette update:modelValue", message: "expected 1 got 0" }], durationMs: 800, at } } },
        { id: "j-run", at, kind: "job", job: job("j-run", "research", { title: "Ricerca: flush dei watch", status: "running", activity: "cerca: vue watch flush post" }) },
        T("t3", "Rispondi pure quando vuoi.", false),
        { id: "fin", at, kind: "finish", result: { lessonPath: "courses/vue-vetrine/lessons/0001-x.html", referencePath: "courses/vue-vetrine/reference/0001-x.html", recordPath: "courses/vue-vetrine/learning-records/0001-x.md", notesUpdated: true, anki: "6 carte sincronizzate" } },
      ], "idle", { plan, exercises: [{ course: "vue-vetrine", name: "06-v-model", dir: "courses/vue-vetrine/exercises/06-v-model", files: ["Esercizio.vue"], instructions: "x" }], lessonPath: "courses/vue-vetrine/lessons/0001-x.html" });
  }
}
