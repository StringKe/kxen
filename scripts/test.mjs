import { readdirSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join, relative } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const coverageEnabled = process.argv.includes("--coverage");
const lineLimit = 350;
const sourceRoots = [
  { directory: join(root, "src-tauri", "src"), extensions: [".rs"] },
  { directory: join(root, "src"), extensions: [".ts", ".tsx"] },
];

function files(directory, extensions) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return files(path, extensions);
    return extensions.some((extension) => entry.name.endsWith(extension)) ? [path] : [];
  });
}

function effectiveLines(path) {
  let count = 0;
  let inBlockComment = false;
  for (const line of readFileSync(path, "utf8").split(/\r?\n/)) {
    let source = line.trim();
    if (inBlockComment) {
      if (!source.includes("*/")) continue;
      inBlockComment = false;
      source = source.split("*/", 2)[1].trim();
      if (!source || source.startsWith("//")) continue;
    }
    if (!source || source.startsWith("//")) continue;
    if (source.startsWith("/*")) {
      if (!source.includes("*/")) inBlockComment = true;
      continue;
    }
    count += 1;
  }
  return count;
}

const lineViolations = sourceRoots
  .flatMap(({ directory, extensions }) => files(directory, extensions))
  .map((path) => ({ path, lines: effectiveLines(path) }))
  .filter(({ lines }) => lines > lineLimit)
  .sort((left, right) => right.lines - left.lines);

if (lineViolations.length > 0) {
  process.stderr.write(`effective-line limit ${lineLimit} exceeded:\n`);
  for (const { path, lines } of lineViolations) {
    process.stderr.write(`  ${String(lines).padStart(5)}  ${relative(root, path)}\n`);
  }
  process.exit(1);
}
process.stdout.write(`effective-line check OK (limit ${lineLimit})\n`);

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: root,
    env: process.env,
    stdio: "inherit",
  });
  if (result.status !== 0) {
    throw new Error(`${command} exited with status ${result.status ?? 1}`);
  }
}

const args = ["exec", "vitest", "run", "--fileParallelism=false", "--maxWorkers=1"];
if (coverageEnabled) {
  args.push(
    "--coverage.enabled",
    "--coverage.provider=istanbul",
    "--coverage.reporter=text",
    "--coverage.reporter=json",
    "--coverage.reportsDirectory=coverage",
    "--coverage.thresholds.lines=80",
    "--coverage.thresholds.functions=80",
    "--coverage.thresholds.statements=80",
    "--coverage.thresholds.branches=70",
  );
}
run("pnpm", args);
