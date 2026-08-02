import { createSignal, onMount, Show } from "solid-js";
import { errText } from "../err-text";
import { availableUpdate, checkForUpdate, currentVersion, installUpdate } from "../../lib/updater";

export default function UpdateSection() {
  const [version, setVersion] = createSignal("");
  const [status, setStatus] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  // 与启动静默检查共用同一状态源：启动已发现的更新进页即见，不重复请求
  const update = availableUpdate;

  onMount(() => {
    void currentVersion()
      .then(setVersion)
      .catch(() => setVersion("UNKNOWN"));
    const found = availableUpdate();
    if (found) setStatus(`发现版本 ${found.version}`);
  });

  const check = async () => {
    setBusy(true);
    setStatus("正在检查更新");
    try {
      const available = await checkForUpdate();
      if (!available) {
        setStatus("当前已是最新版本");
        return;
      }
      setStatus(`发现版本 ${available.version}`);
    } catch (error) {
      setStatus(`检查失败：${errText(error)}`);
    } finally {
      setBusy(false);
    }
  };

  const install = async () => {
    const available = update();
    if (!available) return;
    setBusy(true);
    setStatus(`正在下载并安装 ${available.version}`);
    try {
      await installUpdate(available);
    } catch (error) {
      setStatus(`安装失败：${errText(error)}`);
      setBusy(false);
    }
  };

  return (
    <div class="flex items-center justify-between px-4 py-3">
      <div>
        <div class="text-sm">应用更新</div>
        <div class="text-xs text-[var(--text-faint)]">
          当前版本 {version() || "正在读取"}
          <Show when={status()}>，{status()}</Show>
        </div>
      </div>
      <div class="flex gap-1.5">
        <Show when={update()}>
          <button
            class="pressable px-2.5 py-1 rounded-md text-xs border border-[var(--accent)] text-[var(--accent-hover)]"
            disabled={busy()}
            onClick={() => void install()}
          >
            下载并安装
          </button>
        </Show>
        <button
          class="pressable px-2.5 py-1 rounded-md text-xs border border-[var(--border)] text-[var(--text)]"
          disabled={busy()}
          onClick={() => void check()}
        >
          {busy() ? "处理中" : "检查更新"}
        </button>
      </div>
    </div>
  );
}
