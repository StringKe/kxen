// 手动粘贴凭证表单：API Key tab 直显；OAuth tab 收进「高级：手动粘贴凭证」折叠区。
// OAuth 粘贴的解析警告（缺 refresh_token）/错误（JSON 损坏）就地展示；错误时保存与测试均中止。
import { createSignal, Show } from "solid-js";
import { importAccount, providerVerify } from "../../lib/provider";
import { formatError } from "../../lib/error-text";
import { errText } from "../err-text";
import {
  ACCOUNT_NAME_BAD,
  kind,
  name,
  parseAccountToken,
  resetAccountForm,
  provider,
  setToken,
  token,
} from "./add-account-form";

export default function ManualTokenForm(props: {
  ready: () => boolean;
  region: () => string | undefined;
  onDone: (msg: string) => void;
}) {
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [testing, setTesting] = createSignal(false);
  const [testMsg, setTestMsg] = createSignal<{ ok: boolean; text: string } | null>(null);

  const parsed = () => parseAccountToken(kind(), token());
  const parseIssue = () => {
    if (kind() !== "oauth" || !token().trim()) return null;
    const p = parsed();
    return p.error
      ? { ok: false, text: p.error }
      : p.warning
        ? { ok: true, text: p.warning }
        : null;
  };

  const checkName = (): boolean => {
    if (!name().trim()) {
      setError("账号名必填");
      return false;
    }
    if (ACCOUNT_NAME_BAD.test(name().trim())) {
      setError("名称不能含冒号或空白字符（会成为凭证键的一部分）");
      return false;
    }
    return true;
  };

  // 候选凭证仅进后端内存克隆，不落盘
  const testConn = async () => {
    const p = parsed();
    if (p.error) {
      setError(p.error);
      return;
    }
    if (!p.access) {
      setError("先填凭证再测试连接");
      return;
    }
    setTesting(true);
    setError("");
    setTestMsg(null);
    try {
      const r = await providerVerify(provider(), undefined, {
        access: p.access,
        refresh: p.refresh,
        expires: p.expires,
        kind: kind() === "apikey" ? "api" : "oauth",
        region: props.region(),
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
      if (!token().trim()) {
        setError("凭证必填");
        return;
      }
      const p = parsed();
      if (p.error) {
        setError(p.error);
        return;
      }
      await importAccount(
        provider(),
        name().trim(),
        p.access,
        kind() === "apikey" ? "api" : "oauth",
        p.refresh,
        p.expires,
        props.region(),
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
    <div class="space-y-2">
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
      <Show when={parseIssue()}>
        {(issue) => (
          <div class={`text-xs ${issue().ok ? "text-[var(--warn)]" : "text-[var(--err)]"}`}>
            {issue().text}
          </div>
        )}
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
          disabled={busy() || testing() || !props.ready()}
          onClick={() => void submit()}
        >
          {busy() ? "保存中…" : "保存"}
        </button>
        <button
          class="pressable px-3 py-1 rounded-md text-xs border border-[var(--border)] disabled:opacity-40"
          disabled={busy() || testing() || !props.ready()}
          title="用候选凭证发一次真实最小调用（不保存凭证）"
          onClick={() => void testConn()}
        >
          {testing() ? "测试中…" : "测试连接"}
        </button>
      </div>
    </div>
  );
}
