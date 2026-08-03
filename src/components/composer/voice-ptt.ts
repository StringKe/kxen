// 语音 PTT 状态机：长按空格 >=400ms 进语音（激活期/激活后空格一律 preventDefault 防连打），
// 松开提交；startSession 可注入（测试替身），默认走 RPC 引擎。
import { COMPOSER_INTERRUPT_EVENT } from "../../lib/composer-bus";
import { startVoiceSession, type VoiceSession } from "../../lib/voice";
import { errText } from "../err-text";
import { createTranscriptRange } from "./transcript-range";

export interface VoiceController {
  toggle: () => void;
  /**
   * 停止语音（启动中调用 = 取消启动，等启动落定后自停）。
   * merge（默认）：终稿并入文本（PTT 松开 / 发送前收尾）；
   * discard：丢弃终稿（切会话——base 属旧会话，并入新会话输入框就是串台）。
   */
  stop: (mode?: "merge" | "discard") => Promise<void>;
  /** 等当前主动 stop/final flight 落定；没有 stop 在飞时立即完成。 */
  settle: () => Promise<void>;
  /** 废掉未决的 PTT 激活计时：按住不足 400ms 直接发送时不走 stop，计时留存会在发送后触发开录。 */
  cancelPendingActivation: () => void;
  /** 启动中（权限弹窗/引擎未决）：发送方据此区分「等终稿」还是「取消不等」。 */
  starting: () => boolean;
  onSpaceDown: (e: KeyboardEvent) => void;
  onSpaceUp: (e: KeyboardEvent) => void;
  /** 卸载时摘除监听并丢弃终稿地停止 active/starting session，防后台继续占用麦克风。 */
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
  const transcript = createTranscriptRange(opts);
  // 启动 flight 句柄：stop 在启动中调用时靠它等启动落定，否则取消请求被 start 守卫吞掉
  let startFlight: Promise<void> | null = null;
  let stopFlight: Promise<void> | null = null;
  let pttTimer: ReturnType<typeof setTimeout> | undefined;
  let pttActive = false;
  let spaceCountAtDown = 0;
  let errTimer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;
  // 每次启动和 discard 都换代。旧 partial、启动结果或 stop 终稿只能写回创建它的 generation。
  let generation = 0;

  function clearPttTimer() {
    if (pttTimer) {
      clearTimeout(pttTimer);
      pttTimer = undefined;
    }
  }

  // 错误统一入口：空串即清；非空到时自动消退
  function reportError(msg: string) {
    if (disposed) return;
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
    const currentGeneration = ++generation;
    starting = true;
    cancelled = false;
    reportError("");
    transcript.reset();
    try {
      const s = await startSession(
        opts.engine(),
        (partial) => {
          // 取消/停止后迟到的 partial 不上屏（发送已清空、会话已切换）
          if (cancelled || disposed || currentGeneration !== generation) return;
          transcript.render(partial);
        },
        (msg) => {
          if (disposed || currentGeneration !== generation) return;
          reportError(msg);
          void stop();
        },
        opts.sessionId?.() ?? "",
      );
      if (cancelled || disposed || currentGeneration !== generation) {
        // 启动落定前已被取消（启动中 toggle/send/切会话）：自停；
        // 非卸载取消的停失败上屏；卸载后 UI 已销毁，但仍等待 stop 完成再结束 flight。
        await s.stop().catch((e) => reportError(errText(e)));
        return;
      }
      session = s;
      opts.setRecording(true);
      opts.onStarted?.(s.engine);
    } catch (e) {
      if (!cancelled && !disposed && currentGeneration === generation) reportError(errText(e));
      // 失败复位：PTT 不留激活态（继续按住只剩普通空格键，keyup 自然结束）；
      // 激活计时一并清——repeat 分支靠 pttTimer 判激活期，留着会把继续按住的 repeat 空格全吞掉
      pttActive = false;
      clearPttTimer();
    } finally {
      starting = false;
    }
  }

  function launch() {
    if (disposed || session || starting) return;
    startFlight = start();
  }

  async function stopOnce(mode: "merge" | "discard", currentGeneration: number) {
    // 启动中取消：等启动落定（cancelled 已置，start 落定后自停，session 保持 null）
    if (starting) await startFlight;
    const s = session;
    session = null;
    // starting 状态在卸载后才落定时不再写已销毁 UI；active dispose 仍同步收回 recording。
    if (!disposed || s) opts.setRecording(false);
    if (!s) return;
    const finalText = await s.stop().catch((e) => {
      if (currentGeneration === generation) reportError(errText(e));
      return null;
    });
    // discard（切会话）：终稿属旧会话，落进当前输入框就是串台
    if (mode === "discard" || disposed || currentGeneration !== generation || !finalText) return;
    transcript.render(finalText);
  }

  function stop(mode: "merge" | "discard" = "merge"): Promise<void> {
    if (mode === "discard") generation++;
    const currentGeneration = generation;
    cancelled = true;
    pttActive = false;
    // 停 PTT 必须废掉未决的激活计时：否则计时随后触发 launch，
    // 面板打断/启动中取消等「概念上已结束」的路径会莫名重新开录
    clearPttTimer();
    // 同一 session 已被主动 stop 并等待 final 时，共享原 flight。否则第二次 stop 会因 session 已清空而
    // 提前完成，使紧随 keyup 的 Enter 把 partial 先发出，随后终稿再倒灌输入框。
    if (stopFlight && !session) return stopFlight;
    const flight = stopOnce(mode, currentGeneration).finally(() => {
      if (stopFlight === flight) stopFlight = null;
    });
    stopFlight = flight;
    return flight;
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
      if (disposed) return;
      // starting 也算「已触发」：启动中再按 = 取消
      // （只查 session 会把取消吞掉，权限弹窗期间不可取消）
      if (session || starting) void stop();
      else launch();
    },
    stop,
    settle: () => stopFlight ?? Promise.resolve(),
    cancelPendingActivation: clearPttTimer,
    starting: () => starting,
    onSpaceDown: (e) => {
      if (disposed) return;
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
      if (disposed) return;
      if (e.key !== " ") return;
      releasePtt();
    },
    dispose: () => {
      if (disposed) return;
      disposed = true;
      window.removeEventListener("blur", onWindowBlur);
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener(COMPOSER_INTERRUPT_EVENT, onInterrupt);
      if (errTimer) {
        clearTimeout(errTimer);
        errTimer = undefined;
      }
      // cleanup 不能 await，但 stop 会立即取消计时/partial，并在 start 落定后关闭迟到的 session。
      void stop("discard");
    },
  };
}
