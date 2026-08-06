// 添加账号面板：registry 驱动三类入口；表单状态在 add-account-form.ts，卸载不清半填内容。
// OAuth tab 主操作是应用内登录（OAuthLogin），手贴凭证收进「高级」折叠区（ManualTokenForm）。
import { createSignal, For, onMount, Show } from "solid-js";
import {
  addCustomProvider,
  probeModels,
  providerList,
  type ProviderInfo,
} from "../../lib/provider";
import { flashErr } from "../../lib/flash";
import { errText } from "../err-text";
import { createSeqGuard } from "../../lib/async-guard";
import ProviderRegistryStatus from "./ProviderRegistryStatus";
import OAuthLogin from "./OAuthLogin";
import ManualTokenForm from "./ManualTokenForm";
import {
  ACCOUNT_NAME_BAD,
  baseUrl,
  caps,
  kind,
  models,
  name,
  protocol,
  provider,
  region,
  resetAccountForm,
  setBaseUrl,
  setCaps,
  setKind,
  setModels,
  setName,
  setProtocol,
  setProvider,
  setRegion,
  setToken,
  token,
  type AccountKind,
} from "./add-account-form";

const KINDS: { id: AccountKind; label: string; detail: string }[] = [
  {
    id: "oauth",
    label: "订阅 OAuth",
    detail:
      "Claude / ChatGPT / Gemini / Grok / Kimi / Qwen / MiniMax / OpenRouter 订阅（应用内授权登录）",
  },
  {
    id: "apikey",
    label: "API Key",
    detail: "官方平台 key（DeepSeek / 月之暗面 / 智谱 / 通义 / Mistral / Groq / Gemini 等）",
  },
  { id: "custom", label: "自定义提供商", detail: "OpenAI / Anthropic 兼容端点（中转、自部署）" },
];

const CAPS = ["text", "vision", "audio"];
export default function AddAccountPanel(props: { onDone: (msg: string) => void }) {
  const [providers, setProviders] = createSignal<ProviderInfo[]>([]);
  const [providerLoaded, setProviderLoaded] = createSignal(false);
  const [providerError, setProviderError] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [probing, setProbing] = createSignal(false);
  const [probeMsg, setProbeMsg] = createSignal<{ ok: boolean; text: string } | null>(null);
  const providerGuard = createSeqGuard();

  // oauth tab 列订阅厂商 + 支持应用内 OAuth 登录的 api_key 厂商（如 openrouter 换永久 key）；
  // api key tab 只列手填 key 厂商；local_free 无凭证概念，不进入账号面板。
  const visible = () =>
    providers().filter((p) =>
      kind() === "oauth" ? p.auth === "oauth" || p.oauth_login : p.auth === "api_key",
    );

  const loadProviders = async () => {
    const request = providerGuard.next();
    try {
      const list = await providerList();
      if (!providerGuard.isCurrent(request)) return;
      setProviders(list);
      setProviderError("");
      setProviderLoaded(true);
      if (kind() !== "custom" && !visible().some((p) => p.key === provider())) {
        setProvider(visible()[0]?.key ?? "");
      }
    } catch (error) {
      if (!providerGuard.isCurrent(request)) return;
      const message = errText(error);
      setProviderError(message);
      setProviderLoaded(true);
      flashErr(`加载 provider 清单失败：${message}`);
    }
  };
  onMount(() => void loadProviders());

  const spec = () => providers().find((p) => p.key === provider());
  const regions = () => spec()?.regions ?? [];
  const providerReady = () => kind() === "custom" || (providerLoaded() && visible().length > 0);
  const chosenRegion = () => (regions().length > 1 && region() ? region() : undefined);

  const toggleCap = (c: string) =>
    setCaps((prev) => (prev.includes(c) ? prev.filter((x) => x !== c) : [...prev, c]));

  // 探测自定义端点模型清单（不落盘），成功直接覆盖模型清单输入框
  const probeEndpoint = async () => {
    setProbing(true);
    setProbeMsg(null);
    setError("");
    try {
      const r = await probeModels(baseUrl().trim(), token().trim(), protocol());
      setModels(r.models.join(", "));
      setProbeMsg({ ok: true, text: `已拉取 ${r.models.length} 个模型` });
    } catch (e) {
      setProbeMsg({ ok: false, text: errText(e) });
    } finally {
      setProbing(false);
    }
  };

  const submit = async () => {
    setBusy(true);
    setError("");
    try {
      if (!name().trim()) {
        setError("提供商名必填");
        return;
      }
      if (ACCOUNT_NAME_BAD.test(name().trim())) {
        setError("名称不能含冒号或空白字符（会成为凭证键的一部分）");
        return;
      }
      const doneName = name().trim();
      const list = models()
        .split(/[,，\s]+/)
        .filter(Boolean);
      if (!baseUrl().trim() || list.length === 0 || !token().trim()) {
        setError("base_url / 模型 / key 均必填");
        return;
      }
      await addCustomProvider(
        name().trim(),
        baseUrl().trim(),
        token().trim(),
        list,
        protocol(),
        caps(),
      );
      resetAccountForm();
      props.onDone(`自定义提供商 ${doneName} 已添加`);
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] p-3 space-y-2.5">
      <div class="flex gap-1.5">
        <For each={KINDS}>
          {(k) => (
            <button
              class="pressable px-2.5 py-1 rounded-md text-xs border"
              classList={{
                "border-[var(--accent)] text-[var(--accent-hover)]": kind() === k.id,
                "border-[var(--border)] text-[var(--text-dim)]": kind() !== k.id,
              }}
              title={k.detail}
              onClick={() => {
                setKind(k.id);
                if (k.id !== "custom" && !visible().some((p) => p.key === provider())) {
                  setProvider(visible()[0]?.key ?? "");
                }
              }}
            >
              {k.label}
            </button>
          )}
        </For>
      </div>
      <div class="text-2xs text-[var(--text-faint)]">
        {KINDS.find((k) => k.id === kind())?.detail}
      </div>
      <ProviderRegistryStatus
        loaded={providerLoaded()}
        error={providerError()}
        stale={providers().length > 0}
        onRetry={() => void loadProviders()}
      />

      <Show when={kind() !== "custom"}>
        <div class="flex gap-2">
          <select
            class="form-select"
            value={provider()}
            onChange={(e) => {
              setProvider(e.currentTarget.value);
              setRegion("");
            }}
          >
            <For each={visible()}>{(p) => <option value={p.key}>{p.display}</option>}</For>
          </select>
          <Show when={regions().length > 1}>
            <select
              class="form-select"
              title="运营区域（账号凭证只对该区域端点有效）"
              value={region() || regions()[0]?.key}
              onChange={(e) => setRegion(e.currentTarget.value)}
            >
              <For each={regions()}>
                {(r) => <option value={r.key}>{`${spec()?.display} ${r.display}`}</option>}
              </For>
            </select>
          </Show>
          <input
            class="flex-1 bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs"
            placeholder={
              kind() === "oauth"
                ? "账号名（默认 default；不含冒号空格）"
                : "账号名（如 work / personal；不含冒号空格）"
            }
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
          />
        </div>
        <Show when={kind() === "apikey"}>
          <ManualTokenForm ready={providerReady} region={chosenRegion} onDone={props.onDone} />
        </Show>
        <Show when={kind() === "oauth"}>
          <Show when={provider() === "google-oauth" || provider() === "google-antigravity"}>
            <div class="text-2xs text-[var(--warn)]">
              Google
              限制第三方客户端复用其登录凭证，使用订阅登录存在账号被限制的风险；在意风险请改用 API
              Key 方式接入 Gemini。
            </div>
          </Show>
          <Show when={provider() === "zhipu-coding"}>
            <div class="text-2xs text-[var(--warn)]">
              Z.AI 未开放第三方登录，该流程复用 ZCode 客户端契约，可能随官方调整失效；失败请改用 API
              Key。
            </div>
          </Show>
          <OAuthLogin
            providerKey={provider}
            ready={providerReady}
            onDone={(msg) => {
              resetAccountForm();
              props.onDone(msg);
            }}
          />
          <details class="rounded border border-[var(--border)] px-2 py-1.5">
            <summary class="cursor-pointer text-2xs text-[var(--text-faint)]">
              高级：手动粘贴凭证
            </summary>
            <div class="mt-2">
              <ManualTokenForm ready={providerReady} region={chosenRegion} onDone={props.onDone} />
            </div>
          </details>
        </Show>
      </Show>

      <Show when={kind() === "custom"}>
        <div class="flex gap-2">
          <input
            class="flex-1 bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs"
            placeholder="提供商名（英文，如 my-relay；不含冒号空格）"
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
          />
          <select
            class="form-select"
            value={protocol()}
            onChange={(e) => setProtocol(e.currentTarget.value as "openai" | "anthropic")}
          >
            <option value="openai">openai 协议</option>
            <option value="anthropic">anthropic 协议</option>
          </select>
        </div>
        <input
          class="form-mono"
          placeholder="base_url（远程必须 HTTPS；本机 HTTP 仅 localhost/loopback）"
          value={baseUrl()}
          onInput={(e) => setBaseUrl(e.currentTarget.value)}
        />
        <input
          type="password"
          class="form-mono"
          placeholder="api key（存本机 auth.json，0600）"
          value={token()}
          onInput={(e) => setToken(e.currentTarget.value)}
        />
        <div class="flex items-center gap-2">
          <button
            class="pressable px-3 py-1 rounded-md text-xs border border-[var(--border)] disabled:opacity-40"
            disabled={probing() || !baseUrl().trim() || !token().trim()}
            title="用当前 base_url / key 探测端点模型清单（不保存）"
            onClick={() => void probeEndpoint()}
          >
            {probing() ? "探测中…" : "测试连接并拉取模型"}
          </button>
          <Show when={probeMsg()}>
            {(m) => (
              <span class={`text-xs ${m().ok ? "text-[var(--ok)]" : "text-[var(--err)]"}`}>
                {m().text}
              </span>
            )}
          </Show>
        </div>
        <input
          class="form-mono"
          placeholder="模型清单（逗号分隔，如 gpt-4o, claude-sonnet-4-5）"
          value={models()}
          onInput={(e) => setModels(e.currentTarget.value)}
        />
        <div class="flex items-center gap-3 text-xs text-[var(--text-dim)]">
          能力：
          <For each={CAPS}>
            {(c) => (
              <label class="flex items-center gap-1 cursor-pointer">
                <input type="checkbox" checked={caps().includes(c)} onChange={() => toggleCap(c)} />
                {c}
              </label>
            )}
          </For>
          <span class="text-2xs text-[var(--text-faint)]">audio 可用于语音转写引擎</span>
        </div>
        <Show when={error()}>
          <div class="text-xs text-[var(--err)]">{error()}</div>
        </Show>
        <div class="flex gap-2">
          <button
            class="pressable px-3 py-1 rounded-md text-xs border border-[var(--border)] disabled:opacity-40"
            disabled={busy() || probing()}
            onClick={() => void submit()}
          >
            {busy() ? "保存中…" : "保存"}
          </button>
        </div>
      </Show>
    </div>
  );
}
