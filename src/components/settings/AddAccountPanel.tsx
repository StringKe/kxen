// 添加账号面板：三类入口（订阅 OAuth / API Key / 自定义类型提供商），先选型再填字段。
// provider 与区域下拉来自后端 provider.list（registry 是唯一真相源，前端不硬编码）。
// 表单状态在 add-account-form.ts（模块级）：切设置分区卸载面板不清空半填表单。
import { createSignal, For, onMount, Show } from "solid-js";
import {
  addCustomProvider,
  importAccount,
  providerList,
  providerVerify,
  type ProviderInfo,
} from "../../lib/provider";
import { flashErr } from "../../lib/flash";
import { formatError } from "../../lib/error-text";
import { errText } from "../err-text";
import {
  ACCOUNT_NAME_BAD,
  baseUrl,
  caps,
  kind,
  models,
  name,
  parseAccountToken,
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
    detail: "Claude/ChatGPT/Grok/Kimi 订阅（OAuth JSON 或 access token）",
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
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [testing, setTesting] = createSignal(false);
  const [testMsg, setTestMsg] = createSignal<{ ok: boolean; text: string } | null>(null);

  // tab 过滤 provider 清单：oauth tab 只列订阅厂商，apikey tab 只列官方平台 key 厂商；
  // local_free（ollama）无凭证概念，两个 tab 都不列
  const visible = () =>
    providers().filter((p) => (kind() === "oauth" ? p.auth === "oauth" : p.auth === "api_key"));

  onMount(async () => {
    const list = await providerList().catch((e: unknown) => {
      flashErr(`加载 provider 清单失败：${errText(e)}`);
      return [] as ProviderInfo[];
    });
    setProviders(list);
    if (kind() !== "custom" && !visible().some((p) => p.key === provider())) {
      setProvider(visible()[0]?.key ?? "");
    }
  });

  const spec = () => providers().find((p) => p.key === provider());
  const regions = () => spec()?.regions ?? [];
  // 多区域厂商才带 region；单区域/空选择交给后端缺省
  const chosenRegion = () => (regions().length > 1 && region() ? region() : undefined);

  const toggleCap = (c: string) =>
    setCaps((prev) => (prev.includes(c) ? prev.filter((x) => x !== c) : [...prev, c]));

  const checkName = (): boolean => {
    if (!name().trim()) {
      setError(kind() === "custom" ? "提供商名必填" : "账号名必填");
      return false;
    }
    if (ACCOUNT_NAME_BAD.test(name().trim())) {
      setError("名称不能含冒号或空白字符（会成为凭证键的一部分）");
      return false;
    }
    return true;
  };

  // 保存前的连接实测：复用 provider.verify 链路（候选凭证只进后端内存克隆，不落 auth.json）。
  // custom 无此按钮：自定义端点要 config.toml 落盘后才能解析，保存后用列表行内「实测」验证。
  const testConn = async () => {
    const { access, refresh, expires } = parseAccountToken(kind(), token());
    if (!access) {
      setError("先填凭证再测试连接");
      return;
    }
    setTesting(true);
    setError("");
    setTestMsg(null);
    try {
      const r = await providerVerify(provider(), undefined, {
        access,
        refresh,
        expires,
        kind: kind() === "apikey" ? "api" : "oauth",
        region: chosenRegion(),
      });
      setTestMsg({
        ok: r.ok,
        text: r.ok ? `连接正常 ${(r.latency_ms / 1000).toFixed(1)}s` : formatError(r.detail),
      });
    } catch (e) {
      setTestMsg({ ok: false, text: errText(e) });
    } finally {
      setTesting(false);
    }
  };

  const submit = async () => {
    setBusy(true);
    setError("");
    try {
      if (!checkName()) return;
      if (kind() === "custom") {
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
        return;
      }
      if (!token().trim()) {
        setError("凭证必填");
        return;
      }
      const { access, refresh, expires } = parseAccountToken(kind(), token());
      await importAccount(
        provider(),
        name().trim(),
        access,
        kind() === "apikey" ? "api" : "oauth",
        refresh,
        expires,
        chosenRegion(),
      );
      const doneName = name();
      const doneProvider = provider();
      resetAccountForm();
      props.onDone(`账号 ${doneProvider}:${doneName} 已添加`);
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
                setTestMsg(null);
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
            placeholder="账号名（如 work / personal；不含冒号空格）"
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
          />
        </div>
        <input
          type="password"
          class="form-mono"
          placeholder={
            kind() === "oauth"
              ? "OAuth JSON（access_token/refresh_token/expires_at）或裸 access token"
              : "sk-... API key"
          }
          value={token()}
          onInput={(e) => setToken(e.currentTarget.value)}
        />
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
          placeholder="base_url（API 根，如 https://relay.example.com/v1）"
          value={baseUrl()}
          onInput={(e) => setBaseUrl(e.currentTarget.value)}
        />
        <input
          class="form-mono"
          placeholder="模型清单（逗号分隔，如 gpt-4o, claude-sonnet-4-5）"
          value={models()}
          onInput={(e) => setModels(e.currentTarget.value)}
        />
        <input
          type="password"
          class="form-mono"
          placeholder="api key（存本机 auth.json，0600）"
          value={token()}
          onInput={(e) => setToken(e.currentTarget.value)}
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
      </Show>

      <Show when={error()}>
        <div class="text-xs text-[var(--err)]">{error()}</div>
      </Show>
      <Show when={testMsg()}>
        {(m) => (
          <div class={`text-xs ${m().ok ? "text-[var(--ok)]" : "text-[var(--err)]"}`}>
            {m().ok ? `测试连接：${m().text}` : `测试连接失败：${m().text}`}
          </div>
        )}
      </Show>
      <div class="flex gap-2">
        <button
          class="pressable px-3 py-1 rounded-md text-xs border border-[var(--border)] disabled:opacity-40"
          disabled={busy() || testing()}
          onClick={() => void submit()}
        >
          {busy() ? "保存中…" : "保存"}
        </button>
        <Show when={kind() !== "custom"}>
          <button
            class="pressable px-3 py-1 rounded-md text-xs border border-[var(--border)] disabled:opacity-40"
            disabled={busy() || testing()}
            title="用候选凭证发一次真实最小调用（不保存凭证）"
            onClick={() => void testConn()}
          >
            {testing() ? "测试中…" : "测试连接"}
          </button>
        </Show>
      </div>
    </div>
  );
}
