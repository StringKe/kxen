// 订阅 OAuth 应用内登录：oauth_begin 起会话，code 流展示授权链接（可手贴授权码），
// device 流大字展示 user_code；oauth_wait 立即首查后按间隔轮询，done 交回父组件刷新，
// failed 就地显错回到登录按钮。同一时刻只允许一个会话（会话中登录按钮隐藏）。
// 卸载只清轮询定时器，不擅自取消后端会话（用户可能正在浏览器里授权）。
import { createSignal, onCleanup, Show } from "solid-js";
import { Check, Copy, ExternalLink, RefreshCw } from "lucide-solid";
import {
  oauthBegin,
  oauthCancel,
  oauthWait,
  type OAuthBeginResult,
  type OAuthWaitResult,
} from "../../lib/provider";
import { writeClipboard } from "../../lib/clipboard";
import { errText } from "../err-text";
import { ACCOUNT_NAME_BAD, name } from "./add-account-form";

const DEFAULT_INTERVAL_MS = 2000;

export default function OAuthLogin(props: {
  providerKey: () => string;
  ready: () => boolean;
  onDone: (msg: string) => void;
}) {
  const [session, setSession] = createSignal<OAuthBeginResult | null>(null);
  const [starting, setStarting] = createSignal(false);
  const [error, setError] = createSignal("");
  const [manualCode, setManualCode] = createSignal("");
  const [copied, setCopied] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;
  onCleanup(() => {
    disposed = true;
    stopPoll();
  });

  function stopPoll() {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
  }

  const account = () => name().trim() || "default";

  // 返回值是否 pending：pending 时轮询方继续排下一拍，其余结果定局
  const settle = (r: OAuthWaitResult): boolean => {
    if (disposed) return false;
    if (r.status === "pending") return true;
    stopPoll();
    setSession(null);
    setManualCode("");
    if (r.status === "failed") setError(r.error);
    else props.onDone(`账号 ${props.providerKey()}:${account()} 登录成功`);
    return false;
  };

  const poll = async (s: OAuthBeginResult) => {
    const r = await oauthWait(s.session).catch(
      (e: unknown): OAuthWaitResult => ({ status: "failed", error: errText(e) }),
    );
    // 会话可能已被取消/定局，仅当前会话的 pending 才继续轮询
    if (settle(r) && session()?.session === s.session) {
      timer = setTimeout(() => void poll(s), (s.interval ?? 0) * 1000 || DEFAULT_INTERVAL_MS);
    }
  };

  const begin = async () => {
    setError("");
    if (ACCOUNT_NAME_BAD.test(name().trim())) {
      setError("名称不能含冒号或空白字符（会成为凭证键的一部分）");
      return;
    }
    setStarting(true);
    const s = await oauthBegin(props.providerKey(), account()).catch((e: unknown) => {
      if (!disposed) setError(errText(e));
      return null;
    });
    if (disposed) return;
    setStarting(false);
    if (!s) return;
    setSession(s);
    void poll(s); // 首查不等间隔：code 流用户可能已在浏览器完成授权
  };

  const cancel = async () => {
    const s = session();
    if (!s) return;
    stopPoll();
    setSession(null);
    setManualCode("");
    await oauthCancel(s.session).catch((e: unknown) => {
      if (!disposed) setError(`取消登录失败：${errText(e)}`);
    });
  };

  const submitManual = async () => {
    const s = session();
    const code = manualCode().trim();
    if (!s || !code) return;
    const r = await oauthWait(s.session, code).catch(
      (e: unknown): OAuthWaitResult => ({ status: "failed", error: errText(e) }),
    );
    settle(r); // pending 则交给既有轮询继续等
  };

  const copyUserCode = (code: string) => {
    writeClipboard(code);
    setCopied(true);
    setTimeout(() => {
      if (!disposed) setCopied(false);
    }, 1500);
  };

  return (
    <div class="space-y-2">
      <Show when={!session()}>
        <button
          class="pressable px-3 py-1 rounded-md text-xs border border-[var(--accent)] text-[var(--accent-hover)] disabled:opacity-40"
          disabled={starting() || !props.ready()}
          onClick={() => void begin()}
        >
          {starting() ? "发起登录…" : "登录"}
        </button>
      </Show>
      <Show when={session()}>
        {(s) => (
          <div class="rounded border border-[var(--border)] bg-[var(--bg-overlay)]/50 px-3 py-2 space-y-2">
            <Show when={s().flow === "code"}>
              <div class="text-xs text-[var(--text-dim)]">已打开浏览器，请完成授权</div>
              <Show when={s().authorize_url}>
                {(url) => (
                  <div class="flex items-center gap-2">
                    <span class="text-2xs text-[var(--text-faint)]">浏览器未打开？</span>
                    <code class="flex-1 truncate text-2xs text-[var(--text)] select-all">
                      {url()}
                    </code>
                    <button
                      class="pressable px-2 py-0.5 rounded text-2xs border border-[var(--border)] text-[var(--text-dim)]"
                      onClick={() => writeClipboard(url())}
                    >
                      复制
                    </button>
                  </div>
                )}
              </Show>
              <Show when={s().manual_paste}>
                <div class="flex gap-2">
                  <input
                    class="form-mono"
                    placeholder="粘贴授权码，形如 code#state"
                    value={manualCode()}
                    onInput={(e) => setManualCode(e.currentTarget.value)}
                  />
                  <button
                    class="pressable px-3 py-1 rounded-md text-xs border border-[var(--border)] disabled:opacity-40"
                    disabled={!manualCode().trim()}
                    onClick={() => void submitManual()}
                  >
                    提交
                  </button>
                </div>
              </Show>
            </Show>
            <Show when={s().flow === "device"}>
              <div class="text-xs text-[var(--text-dim)]">请在授权页面输入以下代码：</div>
              <div class="flex items-center gap-2">
                <code
                  class="font-mono text-lg tracking-widest text-[var(--text)] select-all cursor-pointer"
                  title="点击复制"
                  onClick={() => copyUserCode(s().user_code ?? "")}
                >
                  {s().user_code}
                </code>
                <button
                  class="pressable p-1 rounded text-[var(--text-dim)] hover:bg-[var(--bg-overlay)]/60"
                  title="复制代码"
                  onClick={() => copyUserCode(s().user_code ?? "")}
                >
                  {copied() ? <Check size={14} /> : <Copy size={14} />}
                </button>
                <Show when={s().expires_in}>
                  {(secs) => (
                    <span class="text-2xs text-[var(--text-faint)]">
                      {Math.round(secs() / 60)} 分钟内有效
                    </span>
                  )}
                </Show>
              </div>
              <div class="flex items-center gap-2">
                <button
                  class="pressable flex items-center gap-1 px-2 py-0.5 rounded text-2xs border border-[var(--border)] text-[var(--text-dim)]"
                  onClick={() => window.open(s().verification_url, "_blank")}
                >
                  <ExternalLink size={12} />
                  打开授权页面
                </button>
                <code class="flex-1 truncate text-2xs text-[var(--text-faint)] select-all">
                  {s().verification_url}
                </code>
              </div>
            </Show>
            <div class="flex items-center gap-2">
              <span class="flex items-center gap-1 text-2xs text-[var(--text-faint)]">
                <RefreshCw size={11} class="animate-spin" />
                等待授权完成…
              </span>
              <button
                class="pressable px-2 py-0.5 rounded text-2xs border border-[var(--border)] text-[var(--text-dim)]"
                onClick={() => void cancel()}
              >
                取消
              </button>
            </div>
          </div>
        )}
      </Show>
      <Show when={error()}>
        <div class="text-xs text-[var(--err)]">{error()}</div>
      </Show>
    </div>
  );
}
