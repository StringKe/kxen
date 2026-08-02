// 会话树分组名：basename 为主；worktree 目录显示「树名 (worktree)」与项目组区分；撞名上提一级。

/** 路径末段（空路径原样返回）。 */
export function baseName(p: string): string {
  return p.split("/").filter(Boolean).pop() ?? p;
}

export function parentName(p: string): string {
  const segs = p.split("/").filter(Boolean);
  return segs.length >= 2 ? `${segs[segs.length - 2]}/${segs[segs.length - 1]}` : (segs[0] ?? p);
}

/** 解析 kxen worktree 路径（<repo>/.kxen/worktrees/<name>），repo 取 .kxen 的上一级段。 */
export function parseWorktreePath(p: string): { repo: string; name: string } | null {
  const segs = p.split("/").filter(Boolean);
  const n = segs.length;
  if (n < 4 || segs[n - 2] !== "worktrees" || segs[n - 3] !== ".kxen") return null;
  return { repo: segs[n - 4]!, name: segs[n - 1]! };
}

/** 组标题：worktree 目录显示「树名 (worktree)」，否则 basename。 */
export function groupName(p: string): string {
  const wt = parseWorktreePath(p);
  return wt ? `${wt.name} (worktree)` : baseName(p);
}

/** 撞名上提一级：worktree 用「仓库/树名 (worktree)」（parentName 得到 worktrees/<名>，两仓同树名仍撞）。 */
export function promotedName(p: string): string {
  const wt = parseWorktreePath(p);
  return wt ? `${wt.repo}/${wt.name} (worktree)` : parentName(p);
}
