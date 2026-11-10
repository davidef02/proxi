// Downloads the embedding model into .learner/models so search works offline afterwards.
// Layout used by transformers 4.3.2 (both env.cacheDir and env.localModelPath):
// <dir>/<model>/{config.json, tokenizer*, onnx/*.onnx}. The idempotence check below
// must match the one in app/server/graph/embeddings.ts modelPresent().
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { APP_NAME, SKIP_MODEL_ENV } from "../app/shared/identity.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const dir = path.join(ROOT, ".learner", "models");

function present(cfg) {
  const modelDir = path.join(dir, cfg.model);
  const onnxDir = path.join(modelDir, "onnx");
  return (
    fs.existsSync(path.join(modelDir, "config.json")) &&
    fs.existsSync(onnxDir) &&
    fs.readdirSync(onnxDir).some((f) => f.endsWith(".onnx"))
  );
}

async function download(cfg) {
  const { env, pipeline, LogLevel } = await import("@huggingface/transformers");
  env.cacheDir = dir;
  env.localModelPath = dir;
  env.allowRemoteModels = true;
  env.logLevel = LogLevel.ERROR;
  const ex = await pipeline("feature-extraction", cfg.model, { dtype: cfg.dtype });
  await ex(["ok"], { pooling: "mean", normalize: true }); // smoke test: catches a partial download
  await ex.dispose?.();
  console.log(`${APP_NAME}: embedding model ready (${cfg.model})`);
}

try {
  const cfg = JSON.parse(fs.readFileSync(path.join(ROOT, "app/server/graph/embedding-model.json"), "utf8"));
  if (process.env[SKIP_MODEL_ENV] === "1") {
    console.log(`${APP_NAME}: ${SKIP_MODEL_ENV}=1, embedding model download skipped`);
  } else if (!present(cfg)) {
    await download(cfg);
  }
} catch (err) {
  console.warn(
    `${APP_NAME}: embedding model not downloaded (${err?.message ?? err}); semantic search will retry at app start.`,
  );
}
process.exitCode = 0;
