import { spawnSync } from "node:child_process";
import { join } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { enforceChunkBudgets } from "./chunk-budget.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const result = spawnSync("pnpm", ["exec", "vp", "build"], {
  cwd: root,
  env: process.env,
  stdio: "inherit",
});

if (result.status !== 0) {
  process.exit(result.status ?? 1);
}

enforceChunkBudgets(root, join(root, "dist"));
