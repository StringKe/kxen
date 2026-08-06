import { readdirSync, readFileSync, statSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { gzipSync } from "node:zlib";
import { basename, extname, join, relative } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));
const targetName = process.argv[2] ?? "app";
const targets = {
  app: {
    cwd: repoRoot,
    args: ["exec", "vp", "build"],
    output: join(repoRoot, "dist"),
  },
  website: {
    cwd: join(repoRoot, "website"),
    args: ["exec", "astro", "build"],
    output: join(repoRoot, "website", "dist", "_astro"),
  },
};
const target = targets[targetName];

if (!target) {
  throw new Error(
    `unknown build target: ${targetName}; expected one of ${Object.keys(targets).join(", ")}`,
  );
}

const result = spawnSync("pnpm", target.args, {
  cwd: target.cwd,
  env: process.env,
  stdio: "inherit",
});

if (result.status !== 0) {
  process.exit(result.status ?? 1);
}

const DEFAULT_RAW_LIMIT = 500_000;
const MERMAID_PARSER_RAW_LIMIT = 700_000;
const MERMAID_PARSER_GZIP_LIMIT = 150_000;
// @pierre/diffs 的 shiki 语法包/引擎 chunk：按语言异步按需加载（diff 不含该语言则永不拉取），
// TextMate grammar 与 Oniguruma wasm 天然体积大，对齐 mermaid 先例给独立预算。
// 新语言包若超限会在构建期暴露，逐个评审后加名单，不放开通用上限
const SHIKI_ON_DEMAND_RAW_LIMIT = 800_000;
const SHIKI_ON_DEMAND_GZIP_LIMIT = 250_000;

function files(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? files(path) : [path];
  });
}

function budget(path) {
  const name = basename(path);
  if (/^mermaid-parser-runtime[.-]/.test(name)) {
    return {
      raw: MERMAID_PARSER_RAW_LIMIT,
      gzip: MERMAID_PARSER_GZIP_LIMIT,
    };
  }
  if (/^(emacs-lisp|cpp|wasm)[.-]/.test(name)) {
    return {
      raw: SHIKI_ON_DEMAND_RAW_LIMIT,
      gzip: SHIKI_ON_DEMAND_GZIP_LIMIT,
    };
  }
  return { raw: DEFAULT_RAW_LIMIT };
}

const chunks = files(target.output)
  .filter((path) => extname(path) === ".js")
  .map((path) => {
    const size = statSync(path).size;
    const limits = budget(path);
    const gzip = limits.gzip === undefined ? undefined : gzipSync(readFileSync(path)).length;
    return { path, size, gzip, limits };
  })
  .sort((left, right) => right.size - left.size);

for (const chunk of chunks) {
  const gzip = chunk.gzip === undefined ? "" : ` gzip=${chunk.gzip}`;
  process.stdout.write(`${relative(target.cwd, chunk.path)} ${chunk.size} bytes${gzip}\n`);
}

const violations = chunks.filter(
  (chunk) =>
    chunk.size > chunk.limits.raw ||
    (chunk.limits.gzip !== undefined && (chunk.gzip ?? Infinity) > chunk.limits.gzip),
);
if (violations.length > 0) {
  throw new Error(
    `browser runtime chunks exceed budgets: ${violations
      .map(
        (chunk) =>
          `${relative(target.cwd, chunk.path)} raw=${chunk.size}/${chunk.limits.raw}` +
          (chunk.limits.gzip === undefined ? "" : ` gzip=${chunk.gzip}/${chunk.limits.gzip}`),
      )
      .join(", ")}`,
  );
}
