const RPC_TIMEOUT_MS = 30_000;
const APPROVAL_RPC_TIMEOUT_MS = 375_000;

// ApprovalBroker 最长等待 300s；这些 RPC 还需给批准后的 probe/initialize 留出收尾时间。
const APPROVAL_WAIT_METHODS = new Set([
  "config.set_experimental",
  "mcp.auth",
  "mcp.restart",
  "mcp.status",
  "provider.reprobe",
  "worktree.remove",
]);

export function rpcTimeoutMs(method: string): number {
  return APPROVAL_WAIT_METHODS.has(method) ? APPROVAL_RPC_TIMEOUT_MS : RPC_TIMEOUT_MS;
}
