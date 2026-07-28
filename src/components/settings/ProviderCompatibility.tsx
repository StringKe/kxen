import { For } from "solid-js";
import type { ProviderInfo } from "../../lib/provider";

interface Props {
  providers: ProviderInfo[];
}

export default function ProviderCompatibility(props: Props) {
  return (
    <details class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)]">
      <summary class="cursor-pointer px-4 py-2.5 text-xs text-[var(--text-dim)]">
        Provider 兼容性契约（{props.providers.length} 个内置）
      </summary>
      <div class="border-t border-[var(--border)] max-h-72 overflow-auto">
        <For each={props.providers}>
          {(provider) => (
            <div class="grid grid-cols-[1.2fr_1fr_1fr_1.2fr] gap-2 px-4 py-2 text-2xs border-b border-[var(--border)] last:border-b-0">
              <a
                class="text-[var(--accent-hover)] hover:underline truncate"
                href={provider.doc_url}
                target="_blank"
                rel="noreferrer"
              >
                {provider.display}
              </a>
              <span class="font-mono text-[var(--text-dim)]">{provider.protocol}</span>
              <span class="text-[var(--text-dim)]">{provider.auth}</span>
              <span class="text-[var(--text-faint)] truncate" title={provider.default_model}>
                {provider.models_endpoint ? "live /models" : "static catalog"} ·{" "}
                {provider.default_model}
              </span>
            </div>
          )}
        </For>
      </div>
      <div class="px-4 py-2 text-2xs text-[var(--text-faint)] border-t border-[var(--border)]">
        上表是协议契约；账号行的「实测」和「拉模型」才是当前时点的 live 证据。
      </div>
    </details>
  );
}
