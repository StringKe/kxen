/**
 * usage_complete 缺省表示旧后端的精确计量 payload。
 * 显式 false 或存在无法计量调用时，已知 token 只是下限。
 */
export interface UsageCompleteness {
  usage_complete?: boolean;
  unmetered_calls?: number;
  storage_complete?: boolean;
  storage_warning?: string | null;
}

export function hasUnknownStorage(usage?: UsageCompleteness | null): boolean {
  return usage?.storage_complete === false || Boolean(usage?.storage_warning?.trim());
}

export function hasUnknownMetering(usage?: UsageCompleteness | null): boolean {
  if ((usage?.unmetered_calls ?? 0) > 0) return true;
  // 旧后端只有 usage_complete；新后端 false 可能只由 storage degraded 导致。
  return usage?.usage_complete === false && !hasUnknownStorage(usage);
}

export function hasUnknownUsage(usage?: UsageCompleteness | null): boolean {
  return hasUnknownMetering(usage) || hasUnknownStorage(usage);
}

export function usageMeteringUnknownDetail(usage?: UsageCompleteness | null): string {
  const calls = usage?.unmetered_calls ?? 0;
  return calls > 0
    ? `已知 tokens 仅为下限，${calls} 次调用无法计量`
    : "已知 tokens 仅为下限，用量计量不完整";
}

export function usageStorageUnknownDetail(usage?: UsageCompleteness | null): string {
  return usage?.storage_warning?.trim() || "usage.json 持久化状态未知，当前进程内累计尚未确认落盘";
}

export function usageUnknownDetail(usage?: UsageCompleteness | null): string {
  const details = [];
  if (hasUnknownMetering(usage)) details.push(usageMeteringUnknownDetail(usage));
  if (hasUnknownStorage(usage)) details.push(usageStorageUnknownDetail(usage));
  return details.join("；") || "用量完整性未知";
}
