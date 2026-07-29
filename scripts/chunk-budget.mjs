import { readdirSync, readFileSync, statSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { basename, extname, join, relative } from "node:path";

const DEFAULT_RAW_LIMIT = 500_000;
const MERMAID_PARSER_RAW_LIMIT = 700_000;
const MERMAID_PARSER_GZIP_LIMIT = 150_000;

function files(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? files(path) : [path];
  });
}

function budget(path) {
  if (/^mermaid-parser-runtime[.-]/.test(basename(path))) {
    return {
      raw: MERMAID_PARSER_RAW_LIMIT,
      gzip: MERMAID_PARSER_GZIP_LIMIT,
    };
  }
  return { raw: DEFAULT_RAW_LIMIT };
}

export function enforceChunkBudgets(root, directory) {
  const chunks = files(directory)
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
    process.stdout.write(`${relative(root, chunk.path)} ${chunk.size} bytes${gzip}\n`);
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
            `${relative(root, chunk.path)} raw=${chunk.size}/${chunk.limits.raw}` +
            (chunk.limits.gzip === undefined ? "" : ` gzip=${chunk.gzip}/${chunk.limits.gzip}`),
        )
        .join(", ")}`,
    );
  }
}
