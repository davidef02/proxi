import { z } from "zod";
import { browseConcepts, loadCatalog, openConcept, searchConcepts } from "../graph/concepts.ts";
import { rankSemantic } from "../graph/embeddings.ts";
import { fail, tool } from "./util.ts";

interface SearchInput {
  query: string;
  limit?: number;
}
interface OpenInput {
  id: string;
}
interface BrowseInput {
  course?: string;
  lesson?: string;
}

/** Read-only lookups over the knowledge graph; the catalog is rebuilt per call, so nothing is cached across turns. */
export const searchConceptsTool = tool<SearchInput>({
  name: "search_concepts",
  description:
    "Search his knowledge graph by meaning (Italian or English) over name, statement and aliases; falls back to lexical matching when the local model is missing. Returns at most 10 concepts, one per line: id | name | status (solido, fragile, insegnato, mancato) | course | lesson.",
  schema: z.object({
    query: z.string().describe("What the concept is about, in Italian or English (words or a short phrase)."),
    limit: z.number().int().min(1).max(10).optional().describe("Max results, default and cap 10."),
  }) as unknown as z.ZodType<SearchInput>,
  async run(input, ctx) {
    let cat;
    try {
      cat = await loadCatalog(ctx.dataRoot);
    } catch (e) {
      return fail(String((e as Error).message ?? e));
    }
    return searchConcepts(cat, input.query, input.limit, (q, nodes) => rankSemantic(ctx.repoRoot, q, nodes));
  },
});

export const openConceptTool = tool<OpenInput>({
  name: "open_concept",
  description:
    "Open one concept of his knowledge graph: statement, status, courses, lessons, MapNode keys, premises and consequences.",
  schema: z.object({
    id: z.string().describe("Concept id from search_concepts or browse_concepts (n_...)."),
  }) as unknown as z.ZodType<OpenInput>,
  async run(input, ctx) {
    let cat;
    try {
      cat = await loadCatalog(ctx.dataRoot);
    } catch (e) {
      return fail(String((e as Error).message ?? e));
    }
    return openConcept(cat, input.id);
  },
});

export const browseConceptsTool = tool<BrowseInput>({
  name: "browse_concepts",
  description:
    "Browse his knowledge graph: no input lists courses, course lists its lessons, course + lesson lists that lesson's concepts with their status.",
  schema: z.object({
    course: z.string().optional().describe("Course slug; omit to list courses."),
    lesson: z.string().optional().describe("Lesson id <course>/<NNNN-slug> or <NNNN-slug>; needs course."),
  }) as unknown as z.ZodType<BrowseInput>,
  async run(input, ctx) {
    let cat;
    try {
      cat = await loadCatalog(ctx.dataRoot);
    } catch (e) {
      return fail(String((e as Error).message ?? e));
    }
    return browseConcepts(cat, input.course, input.lesson);
  },
});
