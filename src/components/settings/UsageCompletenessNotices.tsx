import { Show } from "solid-js";
import {
  hasUnknownMetering,
  hasUnknownStorage,
  usageMeteringUnknownDetail,
  usageStorageUnknownDetail,
  type UsageCompleteness,
} from "../../lib/usage";

type UsageNotice = UsageCompleteness & { metering_warning?: string | null };

export default function UsageCompletenessNotices(props: { usage: UsageNotice | null }) {
  return (
    <>
      <Show when={hasUnknownMetering(props.usage)}>
        <div
          class="px-4 pb-3 text-2xs text-[var(--warn)]"
          title={usageMeteringUnknownDetail(props.usage)}
        >
          计量 UNKNOWN：{usageMeteringUnknownDetail(props.usage)}
        </div>
      </Show>
      <Show when={hasUnknownStorage(props.usage)}>
        <div
          class="px-4 pb-3 text-2xs text-[var(--warn)]"
          title={usageStorageUnknownDetail(props.usage)}
        >
          存储 UNKNOWN：当前显示包含进程内累计，尚未确认全部写入 usage.json。
          {usageStorageUnknownDetail(props.usage)}
        </div>
      </Show>
      <Show when={props.usage?.metering_warning?.trim()}>
        <div class="px-4 pb-3 text-2xs text-[var(--warn)]">
          趋势 UNKNOWN：{props.usage?.metering_warning}
        </div>
      </Show>
    </>
  );
}
