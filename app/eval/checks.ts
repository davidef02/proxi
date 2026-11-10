// Pure checks on an eval run: no I/O, no engine. app/eval/scenarios.ts pairs them with the prompts.
export type ConceptStatus = "solid" | "taught" | "shaky" | "missed";

export interface FixtureConcept {
  id: string;
  name: string;
  status: ConceptStatus;
  /** Concepts born in the same lesson, or linked by an open_concept chain. */
  group: string;
  /** Only matches when the text exercises the API itself (a call or a directive). */
  usage: RegExp[];
  /** Matches the bare name. */
  mention: RegExp[];
}

export const VUE_CONCEPTS: FixtureConcept[] = [
  { id: "ref", name: "ref", status: "solid", group: "reattivita", usage: [/\bref\s*\(/], mention: [/\bref\b/i, /\.value\b/] },
  { id: "computed", name: "computed", status: "solid", group: "reattivita", usage: [/\bcomputed\s*\(/], mention: [/\bcomputed\b/i] },
  { id: "props", name: "props", status: "taught", group: "contratto", usage: [/\bdefineProps\b/], mention: [/\bprops?\b/i] },
  { id: "emit", name: "emit", status: "taught", group: "contratto", usage: [/\bdefineEmits\b/, /\$?emit\s*\(/], mention: [/\bemit\b/i] },
  { id: "v-model", name: "v-model", status: "taught", group: "contratto", usage: [/\bdefineModel\b/, /v-model\s*=/], mention: [/v-model/i] },
  { id: "watch", name: "watch", status: "shaky", group: "tempo", usage: [/\bwatch(Effect)?\s*\(/], mention: [/\bwatch(Effect)?\b/i] },
  {
    id: "on-mounted",
    name: "on-mounted",
    status: "missed",
    group: "tempo",
    usage: [/\bonMounted\s*\(/],
    mention: [/\bonMounted\b/, /ciclo di vita/i, /lifecycle/i],
  },
];

export const NUXT_PATTERN = /nuxt|useFetch|useAsyncData|useState|\$fetch|nitro|\bssr\b|server|rendering lato server|hydrat|idrata|pages\/|layouts?\b|middleware|routing|\broute|auto-?import|composable|plugin|seo|useHead/i;

export const NUXT_STRANDS: Record<string, RegExp> = {
  routing: /pages\/|routing|\broute|NuxtLink|navigateTo|useRoute/i,
  dati: /useFetch|useAsyncData|\$fetch/i,
  stato: /useState/i,
  rendering: /\bssr\b|lato server|hydrat|idrata|rendering|universal/i,
  layout: /layouts?\b|NuxtLayout/i,
  middleware: /middleware/i,
  "auto-import": /auto-?import|composable/i,
  plugin: /plugin/i,
  seo: /seo|useHead|useSeoMeta/i,
  "server-api": /nitro|server\/api|defineEventHandler/i,
};

export interface ProbeLog {
  question: string;
  options: string[];
  answer: string;
}

const hits = (patterns: RegExp[], text: string) => patterns.some((re) => re.test(text));

export function usageTargetsOf(text: string): FixtureConcept[] {
  return VUE_CONCEPTS.filter((c) => hits(c.usage, text));
}

export function targetsOf(text: string): FixtureConcept[] {
  return VUE_CONCEPTS.filter((c) => hits(c.usage, text) || hits(c.mention, text));
}

export function isPureVue(text: string): boolean {
  return !NUXT_PATTERN.test(text);
}

export function strandsOf(text: string): string[] {
  return Object.keys(NUXT_STRANDS).filter((key) => NUXT_STRANDS[key].test(text));
}

/** A probe may not exercise a solid concept: that is proving what a passed card already proved. */
export function checkSolidSkipped(probes: ProbeLog[]): string[] {
  const errors: string[] = [];
  for (const p of probes) {
    const targets = targetsOf(p.question);
    const solidUsage = usageTargetsOf(p.question).some((c) => c.status === "solid");
    // Nuxt wording excuses a bare mention (the lesson is about Nuxt), never a usage hit.
    const solidMention = isPureVue(p.question) && targets.length > 0 && targets.every((c) => c.status === "solid");
    if (solidUsage || solidMention) errors.push(`solid concept probed: "${p.question}"`);
  }
  return errors;
}

/** At most one confirmation question per group of taught concepts. */
export function checkTaughtGroups(probes: ProbeLog[]): string[] {
  const groups = new Map<string, Set<string>>();
  const taught = new Set<string>();
  for (const c of VUE_CONCEPTS) {
    groups.set(c.group, (groups.get(c.group) ?? new Set()).add(c.id));
    if (c.status === "taught") taught.add(c.id);
  }
  const errors: string[] = [];
  for (const [group, ids] of groups) {
    if ([...ids].some((id) => !taught.has(id))) continue;
    const count = probes.filter((p) => {
      if (!isPureVue(p.question)) return false;
      const targets = targetsOf(p.question);
      return targets.length > 0 && targets.every((c) => ids.has(c.id));
    }).length;
    if (count > 1) errors.push(`group ${group} confirmed ${count} times`);
  }
  return errors;
}

/** No lesson whose subject is a solid concept: Nuxt wording does not excuse it in a plan. */
export function checkCurriculum(titles: string[]): string[] {
  const errors: string[] = [];
  for (const title of titles) {
    const targets = targetsOf(title);
    const solidUsage = usageTargetsOf(title).some((c) => c.status === "solid");
    const solidOnly = targets.length > 0 && targets.every((c) => c.status === "solid");
    if (solidUsage || solidOnly) errors.push(`lesson on a solid concept: "${title}"`);
  }
  return errors;
}

/** The probe must span the subject, not circle one API. */
export function checkStrands(probes: ProbeLog[], min: number): string[] {
  const names = [...new Set(probes.flatMap((p) => strandsOf(p.question)))].sort();
  if (names.length >= min) return [];
  return [`probes cover ${names.length} Nuxt strands (${names.length ? names.join(", ") : "none"}), need ${min}`];
}

export function checkMinProbes(probes: ProbeLog[], min: number): string[] {
  return probes.length >= min ? [] : [`only ${probes.length} probes, need ${min}`];
}
