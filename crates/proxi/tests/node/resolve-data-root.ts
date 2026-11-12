import { resolveDataRoot } from "../../../../extensions/data-root.ts";

const [repo, home] = process.argv.slice(2);
const { dataRoot, source } = resolveDataRoot(repo, process.env, home);
console.log(`${dataRoot} (${source})`);
