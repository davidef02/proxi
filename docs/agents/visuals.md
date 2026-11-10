# Visual Generation & Verification Guidelines

---

## Visual Type Selection

Choose the visual maker based on the nature of the concept:

| Diagram Category | Recommended Maker | Best Fits |
| :--- | :--- | :--- |
| **Relational / Structural** | `mermaid-maker` | Dependency graphs, flowcharts, state machines, sequence diagrams, trees, ER models, timelines. |
| **Spatial / Geometric** | `svg-maker` | Coordinate geometry, number lines, vector plots, function graphs, physical layouts, custom exact shapes. |

---

## The Render-and-Inspect Loop

Correctness is non-negotiable. A visual that asserts something false corrupts the learner's mental model.

1. **Brief the Maker**: Provide a minimal, concrete brief with the fewest elements needed (avoid cramming > 7 nodes).
2. **Author Source**: The maker drafts Mermaid syntax or hand-coded SVG.
3. **Render Image**: The maker compiles the source into a PNG preview using local tools (`@mermaid-js/mermaid-cli` / `rsvg-convert`).
4. **Visual Inspection**: The maker visually inspects the rendered PNG to verify:
   - Arrow directions and dependencies are strictly true.
   - Labels and coordinates are unambiguous and legible.
   - Elements do not overlap, clip, or crowd.
5. **Publish & Return**: The maker saves the final PNG to `./viz/` and returns the file path.

`./viz/` is repo-root on purpose, not under `<dataRoot>/courses/<course>/`: the legacy Pi makers
published there, and on that runtime the folder sat inside the Obsidian vault so embeds resolved by
filename. It is the one artifact a course does not own.

---

## Embedding in Lessons & Notes

- **HTML Lessons**: Embed inline SVG code directly into `<figure>` tags, or link to the verified PNG in `./viz/`.
- **Mermaid DAGs & Dependency Graphs**: Embed raw Mermaid code blocks inside `<pre class="mermaid">` for interactive client-side rendering.
  - Node numbering and label wording: [`pedagogy.md`](pedagogy.md) Phase 2 and § Voice.
