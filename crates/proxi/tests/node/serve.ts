import { createServer } from "../../../../app/server/main.ts";

const [repo] = process.argv.slice(2);
const srv = await createServer({
  repoRoot: repo,
  dataRoot: repo,
  port: 0,
  openBrowser: false,
  checkUpdateOnStart: false,
  fetchModelOnStart: false,
});
console.log(`PORT ${srv.port}`);
