// @pierre/trees 的 Solid 挂载壳：改动文件的树视图（目录聚合 + Git 状态行信号），
// 取代原来的平铺列表。path-first 模型与 diff RPC 的路径口径天然对齐：
// 选中路径即加载该文件 diff，树本身只做展示与选择，不持有数据真相。
// 库本体动态 import 按需加载，不进主 chunk（构建有 500KB 预算门禁）。
import { createEffect, onCleanup, onMount } from "solid-js";
import type { FileTree, GitStatus } from "@pierre/trees";

export interface ChangesTreeEntry {
  path: string;
  status: GitStatus;
  /** 行尾注解（如 `+12 -3`），经 renderRowDecoration 渲染 */
  stats?: string | undefined;
}

export default function ChangesTree(props: {
  entries: () => ChangesTreeEntry[];
  onSelect: (path: string) => void;
}) {
  let ref: HTMLDivElement | undefined;
  let tree: FileTree | undefined;
  let disposed = false;

  onMount(async () => {
    if (!ref) return;
    const container = ref;
    const { FileTree } = await import("@pierre/trees");
    // await 期间组件已卸载：不得再触碰 DOM
    if (disposed) return;
    tree = new FileTree({
      paths: props.entries().map((e) => e.path),
      gitStatus: props.entries().map((e) => ({ path: e.path, status: e.status })),
      flattenEmptyDirectories: true,
      initialExpansion: "open",
      density: "compact",
      onSelectionChange: (paths) => {
        const selected = paths[0];
        if (selected) props.onSelect(selected);
      },
      renderRowDecoration: ({ row }) => {
        const stats = props.entries().find((e) => e.path === row.path)?.stats;
        return stats ? { text: stats } : null;
      },
    });
    tree.render({ containerWrapper: container });
  });

  // 改动清单是 3s 轮询口径：整表替换路径与状态，选中/展开态由树按路径自行保持
  createEffect(() => {
    const entries = props.entries();
    tree?.resetPaths(entries.map((e) => e.path));
    tree?.setGitStatus(entries.map((e) => ({ path: e.path, status: e.status })));
  });

  onCleanup(() => {
    disposed = true;
    tree?.cleanUp();
  });

  // 虚拟化需要确定高度：按可见行数（文件 + 目录链头，对齐 flattenEmptyDirectories 的压缩规则）
  // 估算并封顶，超出由树内滚动承担；compact 密度行高 24px
  const height = () =>
    `${Math.min(estimateRows(props.entries().map((e) => e.path)) * 24 + 8, 224)}px`;
  return <div ref={(el) => (ref = el)} class="changes-tree min-w-0" style={{ height: height() }} />;
}

/** 估算树的可见行数：文件各占一行；目录按 flattenEmptyDirectories 规则计链头（父目录有多个子项或位于根的目录才新开一行）。 */
export function estimateRows(paths: readonly string[]): number {
  const children = new Map<string, Set<string>>();
  for (const p of paths) {
    const segs = p.split("/");
    for (let i = 0; i < segs.length; i++) {
      const parent = i === 0 ? "" : segs.slice(0, i).join("/");
      const child = segs.slice(0, i + 1).join("/");
      let set = children.get(parent);
      if (!set) children.set(parent, (set = new Set()));
      set.add(child);
    }
  }
  let rows = Math.max(paths.length, 1);
  for (const dir of children.keys()) {
    if (dir === "") continue;
    const parent = dir.split("/").slice(0, -1).join("/");
    // 根下目录必占一行；父目录只有一个孩子时单子目录链被压缩进父行，不另占一行
    if (parent === "" || (children.get(parent)?.size ?? 0) > 1) rows++;
  }
  return rows;
}
