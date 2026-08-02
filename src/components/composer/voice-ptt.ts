// 语音 PTT 状态机：长按空格 >=400ms 进语音（激活期/激活后空格一律 preventDefault 防连打），
// 松开提交；startSession 可注入（测试替身），默认走 RPC 引擎。
import { COMPOSER_INTERRUPT_EVENT } from "../../lib/composer-bus";
import { startVoiceSession, type VoiceSession } from "../../lib/voice";
import { errText } from "../err-text";

export interface VoiceController {
  toggle: () => void;
  /**
   * 停止语音（启动中调用 = 取消启动，等启动落定后自停）。
   * merge（默认）：终稿并入文本（PTT 松开 / 发送前收尾）；
   * discard：丢弃终稿（切会话——base 属旧会话，并入新会话输入框就是串台）。
   */
  stop: (mode?: "merge" | "discard") => Promise<void>;
  /** 废掉未决的 PTT 激活计时：按住不足 400ms 直接发送时不走 stop，计时留存会在发送后触发开录。 */
  cancelPendingActivation: () => void;
  /** 启动中（权限弹窗/引擎未决）：发送方据此区分「等终稿」还是「取消不等」。 */
  starting: () => boolean;
  onSpaceDown: (e: KeyboardEvent) => void;
  onSpaceUp: (e: KeyboardEvent) => void;
  /** 卸载时摘除 window 级监听（blur/visibilitychange/浮层打断）与错误消退计时，防重挂载后重复挂听。 */
  dispose: () => void;
}

type StartSession = (
  engine: string | undefined,
  onPartial: (text: string) => void,
  onError: (msg: string) => void,
  sessionId: string,
) => Promise<VoiceSession>;

// 权限类错误只报原因用户不知道去哪修：补下一步指引；后端已带指引（含「系统设置」）的不重复追加
function withVoiceHint(msg: string): string {
  if (!/权限|授权|permission|麦克风|microphone/i.test(msg)) return msg;
  if (/系统设置|settings/i.test(msg)) return msg;
  return `${msg}（前往 系统设置 > 隐私与安全性 开启麦克风/语音识别后重试）`;
}

export function createVoicePtt(opts: {
  getText: () => string;
  setText: (v: string) => void;
  afterChange: () => void;
  setRecording: (v: boolean) => void;
  setError: (v: string) => void;
  engine: () => string;
  startSession?: StartSession;
  /** 当前 chat session id：后端按它键控录音槽位，多会话并发 PTT 互不打断。 */
  sessionId?: () => string;
  /** 启动成功回调：回传实际引擎（降级链可能落到非主引擎）。 */
  onStarted?: (engine: string) => void;
  /** 错误自动消退毫秒数（测试注入小值），缺省 5000：常驻红字会被当成「一直坏着」。 */
  errTtlMs?: number;
}): VoiceController {
  const startSession: StartSession = opts.startSession ?? startVoiceSession;
  let session: VoiceSession | null = null;
  let starting = false;
  let cancelled = false;
  let base = "";
  // 已上屏 partial 长度：新 partial 只替换尾部该区间，保住录音中手打的内容
  let partialLen = 0;
  // 启动 flight 句柄：stop 在启动中调用时靠它等启动落定，否则取消请求被 start 守卫吞掉
  let startFlight: Promise<void> | null = null;
  let pttTimer: ReturnType<typeof setTimeout> | undefined;
  let pttActive = false;
  let spaceCountAtDown = 0;
  let errTimer: ReturnType<typeof setTimeout> | undefined;

  function clearPttTimer() {
    if (pttTimer) {
      clearTimeout(pttTimer);
      pttTimer = undefined;
    }
  }

  // 错误统一入口：空串即清；非空到时自动消退
  function reportError(msg: string) {
    if (errTimer) clearTimeout(errTimer);
    if (!msg) {
      opts.setError("");
      return;
    }
    opts.setError(withVoiceHint(msg));
    errTimer = setTimeout(() => opts.setError(""), opts.errTtlMs ?? 5000);
  }

  async function start() {
    if (session || starting) return;
    starting = true;
    cancelled = false;
    reportError("");
    base = opts.getText();
    partialLen = 0;
    try {
      const s = await startSession(
        opts.engine(),
        (partial) => {
          // 取消/停止后迟到的 partial 不上屏（发送已清空、会话已切换）
          if (cancelled) return;
          // 只替换上次上屏的 partial 区间，其后手打的内容保留
          const tail = opts.getText().slice(base.length + partialLen);
          partialLen = partial.length;
          opts.setText(base + partial + tail);
          opts.afterChange();
        },
        (msg) => {
          reportError(msg);
          void stop();
        },
        opts.sessionId?.() ?? "",
      );
      if (cancelled) {
        // 启动落定前已被取消（启动中 toggle/send/切会话）：自停；
        // 停失败必须上报——引擎停不掉会一直占麦
        s.stop().catch((e) => reportError(errText(e)));
        return;
      }
      session = s;
      opts.setRecording(true);
      opts.onStarted?.(s.engine);
    } catch (e) {
      reportError(errText(e));
      // 失败复位：PTT 不留激活态（继续按住只剩普通空格键，keyup 自然结束）；
      // 激活计时一并清——repeat 分支靠 pttTimer 判激活期，留着会把继续按住的 repeat 空格全吞掉
      pttActive = false;
      clearPttTimer();
    } finally {
      starting = false;
    }
  }

  function launch() {
    if (session || starting) return;
    startFlight = start();
  }

  async function stop(mode: "merge" | "discard" = "merge") {
    cancelled = true;
    pttActive = false;
    // 停 PTT 必须废掉未决的激活计时：否则计时随后触发 launch，
    // 面板打断/启动中取消等「概念上已结束」的路径会莫名重新开录
    clearPttTimer();
    // 启动中取消：等启动落定（cancelled 已置，start 落定后自停，session 保持 null）
    if (starting) await startFlight;
    const s = session;
    session = null;
    opts.setRecording(false);
    const plen = partialLen;
    partialLen = 0;
    if (!s) return;
    const finalText = await s.stop().catch((e) => {
      reportError(errText(e));
      return null;
    });
    // discard（切会话）：终稿属旧会话，落进当前输入框就是串台
    if (mode === "discard" || !finalText) return;
    // 终稿替换 partial 区间，保住录音中手打的内容（同 partial 上屏规则）
    const tail = opts.getText().slice(base.length + plen);
    opts.setText(base + finalText + tail);
    opts.afterChange();
  }

  // 失焦/切后台视同 keyup：窗口失焦后空格 keyup 丢失，pttActive 卡 true 会把之后所有空格吞掉
  function releasePtt() {
    clearPttTimer();
    if (pttActive) {
      pttActive = false;
      void stop();
    }
  }
  const onWindowBlur = () => releasePtt();
  const onVisibility = () => {
    if (document.hidden) releasePtt();
  };
  // 浮层（Cmd-K 面板）打开：焦点被抢后空格 keyup 落进浮层 input，PTT 永远收不到松开
  const onInterrupt = () => void stop();
  window.addEventListener("blur", onWindowBlur);
  document.addEventListener("visibilitychange", onVisibility);
  window.addEventListener(COMPOSER_INTERRUPT_EVENT, onInterrupt);

  return {
    toggle: () => {
      // starting 也算「已触发」：启动中再按 = 取消
      // （只查 session 会把取消吞掉，权限弹窗期间不可取消）
      if (session || starting) void stop();
      else launch();
    },
    stop,
    cancelPendingActivation: clearPttTimer,
    starting: () => starting,
    onSpaceDown: (e) => {
      if (e.key !== " ") return;
      // PTT 已激活或启动中：空格一律不入字（防连打）
      if (pttActive || session || starting) {
        e.preventDefault();
        return;
      }
      if (e.repeat) {
        // 激活期（0-400ms）内的自动重复同样不入字
        if (pttTimer) e.preventDefault();
        return;
      }
      spaceCountAtDown = opts.getText().length;
      pttTimer = setTimeout(() => {
        pttActive = true;
        // 撤销激活期误输入的空格再进语音
        if (opts.getText().length > spaceCountAtDown) {
          opts.setText(opts.getText().slice(0, spaceCountAtDown));
          opts.afterChange();
        }
        launch();
      }, 400);
    },
    onSpaceUp: (e) => {
      if (e.key !== " ") return;
      releasePtt();
    },
    dispose: () => {
      window.removeEventListener("blur", onWindowBlur);
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener(COMPOSER_INTERRUPT_EVENT, onInterrupt);
      if (errTimer) clearTimeout(errTimer);
    },
  };
}
