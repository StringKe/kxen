// TextComposer：Cline 式 textarea 整卡输入（IME/undo/选区全原生免疫）。
// @/# 任意位置 + / 行首触发弹层（光标前切片判定）+ 框外 row chip + 大粘贴折叠占位 + 语音 PTT + 每会话草稿。
import { createEffect, createSignal, Show, onCleanup, onMount } from "solid-js";
import { Send, Square } from "lucide-solid";
import { commandList, type CommandInfo, type ContextItem } from "../../lib/chat";
import { activeSessionId } from "../../lib/state";
import { clearDraft, getDraft, setDraft, stripTruncMark } from "../../lib/drafts";
import { createInFlight, createSeqGuard } from "../../lib/async-guard";
import { flashErr } from "../../lib/flash";
import { errText } from "../err-text";
import { COMPOSER_INSERT_EVENT, COMPOSER_INTERRUPT_EVENT } from "../../lib/composer-bus";
import { detectTrigger, type PopupState, type Trigger } from "./triggers";
import { createAttachments } from "./composer-attachments";
import { createVoicePtt } from "./voice-ptt";
import { caretPopupPos } from "./caret";
import { createPasteStore, planPaste } from "./paste";
import { createTokenEstimate } from "./token-estimate";
import { listenComposerDragDrop } from "./drag-drop";
import { createTriggerCheck } from "./trigger-check";
import { handlePopupKey } from "./popup-keys";
import { buildSendParts } from "./send-payload";
import AttachMenu from "./AttachMenu";
import ComposerPopup from "./ComposerPopup";
import MicControl from "./MicControl";
import ModelPicker from "./ModelPicker";
import RowChips, { type RowChip } from "./RowChips";
import { sendBtn } from "../../lib/variants";

let chipSeq = 0;
const MAX_HEIGHT = 176; // styles 里 max-h-44 同值

export default function TextComposer(props: {
  streaming: () => boolean;
  onSend: (
    text: string,
    context: ContextItem[],
    images: Array<{ media_type: string; data: string }>,
  ) => void;
  onStop: () => void;
  focusTick: () => number;
}) {
  const [text, setText] = createSignal("");
  const [popup, setPopup] = createSignal<(PopupState & Trigger) | null>(null);
  const [popupPos, setPopupPos] = createSignal<{ left: number; bottom: number } | null>(null);
  const [commands, setCommands] = createSignal<CommandInfo[]>([]);
  const [commandsErr, setCommandsErr] = createSignal("");
  const [rowChips, setRowChips] = createSignal<RowChip[]>([]);
  const [recording, setRecording] = createSignal(false),
    [activeVoice, setActiveVoice] = createSignal("");
  const [voiceError, setVoiceError] = createSignal(""),
    // 空 = 不带 engine override：PTT 走后端 config.voice.engine（设置页主引擎）；
    // 仅 MicMenu 显式点选后才有值，作为一次性 override（MicMenu 已同步落后端配置）
    [voiceEngine, setVoiceEngine] = createSignal(""),
    [dragOver, setDragOver] = createSignal(false);
  let ta: HTMLTextAreaElement | undefined;
  let imeLockUntil = 0; // Safari compositionend 先于 commit keydown（WebKit #165231），50ms 锁窗吞尾随 Enter
  const images = new Map<string, { media_type: string; data: string }>();
  const pastes = createPasteStore();
  const commandsGuard = createSeqGuard();

  const reloadCommands = async () => {
    const request = commandsGuard.next();
    try {
      const next = await commandList();
      if (!commandsGuard.isCurrent(request)) return;
      setCommands(next);
      setCommandsErr("");
    } catch (error) {
      if (commandsGuard.isCurrent(request)) setCommandsErr(errText(error));
    }
  };

  const { estimate, estimateCls } = createTokenEstimate(text, () => activeSessionId());
  const cardCls = () => ({ recording: recording(), "drag-over": dragOver() });

  function autogrow() {
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = `${Math.min(ta.scrollHeight, MAX_HEIGHT)}px`;
  }

  function setValue(v: string, caret?: number) {
    if (!ta) return;
    ta.value = v;
    setText(v);
    // 程序化改文本（语音上屏/触发词删除/草稿恢复）与键盘输入同等待遇：落每会话草稿，切会话不丢
    setDraft(activeSessionId(), v);
    const pos = caret ?? v.length;
    ta.setSelectionRange(pos, pos);
    autogrow();
  }

  function insertAtCaret(insert: string) {
    if (!ta) return;
    const pos = ta.selectionStart;
    ta.setRangeText(insert, pos, ta.selectionEnd, "end");
    setText(ta.value);
    // 同 setValue：光标处插入（弹层 apply/总线插入）也落草稿
    setDraft(activeSessionId(), ta.value);
    autogrow();
  }

  /** 删除触发词文本（@xxx / /xxx / #xxx 段），光标归位到删除点。 */
  function removeTriggerText(trigger: Trigger, from?: number) {
    const start = from ?? trigger.start;
    // 定界扫触发段（触发符到下个空白）而非光标位置：光标可能已移出触发段，按光标 slice 会重复中段
    const t = text();
    let end = trigger.start + 1;
    while (end < t.length && !" \t\n　".includes(t[end]!)) end++;
    setValue(t.slice(0, start) + t.slice(end), start);
  }

  /** 光标移出触发段即关弹层：click/方向键/Home/End 等不走 input 的位移也要判。 */
  function closePopupIfCaretOut() {
    const p = popup();
    if (!p || !ta) return;
    // 按光标实时重算触发段：拿建弹层时的 stale query 长度判界，继续打 query 会误判移出把弹层关了
    const t = detectTrigger(text(), ta.selectionStart);
    if (!t || t.start !== p.start) setPopup(null);
  }

  onMount(() => {
    const onInsert = (e: Event) => {
      insertAtCaret((e as CustomEvent<string>).detail);
      ta?.focus();
    };
    window.addEventListener(COMPOSER_INSERT_EVENT, onInsert);
    const onInterrupt = () => setPopup(null);
    window.addEventListener(COMPOSER_INTERRUPT_EVENT, onInterrupt);
    onCleanup(() => window.removeEventListener(COMPOSER_INSERT_EVENT, onInsert));
    onCleanup(() => window.removeEventListener(COMPOSER_INTERRUPT_EVENT, onInterrupt));
    onCleanup(listenComposerDragDrop(setDragOver, (paths) => void attachPaths(paths)));
    ta?.focus();
  });

  createEffect(() => {
    props.focusTick();
    activeSessionId();
    void reloadCommands();
    // 切会话：停掉在录/启动中的语音，终稿 discard——base 属旧会话，落进新会话输入框是串台；
    // 旧会话已上屏的 partial 不走终稿，草稿已随 setValue 持续落盘，不丢
    void voiceCtl.stop("discard");
    // 每会话草稿：切走前已持续落盘，切回恢复；row chip 不跨会话保留
    const d = getDraft(activeSessionId());
    setRowChips([]);
    images.clear();
    pastes.clear();
    setPopup(null);
    setValue(stripTruncMark(d));
    ta?.focus();
  });

  const voiceCtl = createVoicePtt({
    getText: () => text(),
    setText: (v) => setValue(v),
    afterChange: () => {},
    setRecording,
    setError: setVoiceError,
    engine: voiceEngine,
    sessionId: () => activeSessionId(),
    onStarted: setActiveVoice,
  });
  onCleanup(voiceCtl.dispose);

  function updatePopupPos() {
    setPopupPos(caretPopupPos(ta));
  }

  const pushChip = (chip: Omit<RowChip, "id">) =>
    setRowChips((prev) => [...prev, { id: `chip_${chipSeq++}`, ...chip }]);

  const removeChip = (id: string) => {
    const chip = rowChips().find((c) => c.id === id);
    // 图片 base64 随 chip 释放：images Map 只增不清会把已删图片带进后续发送
    if (chip?.kind === "image") images.delete(chip.ref);
    setRowChips((prev) => prev.filter((c) => c.id !== id));
  };

  // hover 与键盘选中合一：写同一个 selected，谁后动谁生效
  const syncSelected = (i: number) =>
    setPopup((p) => (p && p.selected !== i ? { ...p, selected: i } : p));

  const triggerCheck = createTriggerCheck({
    ta: () => ta,
    text,
    commands,
    commandsError: commandsErr,
    retryCommands: reloadCommands,
    removeTriggerText,
    pushChip,
    insertAtCaret,
    setPopup,
    updatePopupPos,
  });
  onCleanup(triggerCheck.dispose);
  createEffect(() => {
    commands();
    commandsErr();
    triggerCheck.run();
  });

  const { attachFiles, attachPaths } = createAttachments({
    images,
    pushChip,
    scope: activeSessionId,
  });

  function onPaste(e: ClipboardEvent) {
    const { files, text, manual, large } = planPaste(e);
    if (files) attachFiles(files);
    if (files || manual) e.preventDefault();
    if (manual) insertAtCaret(large ? pastes.add(text) : text);
  }

  function onKeyDown(e: KeyboardEvent) {
    const p = popup();
    // IME 组字中弹层放行：Enter/方向键归输入法候选窗（isComposing/keyCode229/锁窗三保险，同发送守卫）
    if (p && (e.isComposing || e.keyCode === 229 || Date.now() < imeLockUntil)) return;
    if (p && handlePopupKey(e, p, setPopup)) return;
    // IME 提交 Enter 不发送：isComposing / keyCode 229 / 50ms 锁窗 三保险（cline#3475 同款）
    if (
      e.key === "Enter" &&
      !e.shiftKey &&
      !e.isComposing &&
      e.keyCode !== 229 &&
      Date.now() >= imeLockUntil
    ) {
      e.preventDefault();
      sendGuarded();
      return;
    }
    voiceCtl.onSpaceDown(e);
  }

  async function send() {
    voiceCtl.cancelPendingActivation(); // 快速 Enter（按住不足 400ms）不走 stop：废未决激活计时，防发送后触发开录
    // 录音中发送：先等语音收尾（终稿并入输入框），连终稿一起发——
    // 不 await 发出去的是旧 partial，终稿会倒灌已清空的输入框。
    // 仅启动中（权限弹窗未决）取消不等待：此刻没有终稿可等，发送不能被弹窗卡住。
    if (recording()) await voiceCtl.stop();
    else if (voiceCtl.starting()) void voiceCtl.stop();
    const value = pastes.expand(text()).trim();
    // err chip 只是装配失败的告示（可点 X 移除），不进发送载荷：仅剩 err chip 时按空输入处理
    const payloadChips = rowChips().filter((c) => c.kind !== "err");
    if (!value && payloadChips.length === 0) return;
    const { context, imageParts } = buildSendParts(payloadChips, images);
    props.onSend(value, context, imageParts);
    pastes.clear();
    setValue("", 0);
    // setValue 会落草稿，清草稿必须在其后，否则空串又写回去
    clearDraft(activeSessionId());
    setRowChips([]);
    images.clear(); // 图片数据已随 imageParts 消费，不留到下一轮
  }

  // 等语音终稿期间连按 Enter/连点发送键不得双发：in-flight 去重共享同一 Promise
  const sendDedupe = createInFlight();
  const sendGuarded = () => {
    void sendDedupe("send", send).catch((e) => {
      flashErr(`发送失败：${errText(e)}`);
    });
  };

  return (
    <div class="relative">
      <Show when={popup()}>
        {(p) => (
          <ComposerPopup
            items={p().items}
            selected={p().selected}
            pos={popupPos()}
            onHover={syncSelected}
          />
        )}
      </Show>
      <div class="composer-card rounded-xl relative" classList={cardCls()}>
        <RowChips chips={rowChips()} onRemove={removeChip} />
        <textarea
          ref={(el) => (ta = el)}
          rows={1}
          class="w-full resize-none bg-transparent px-3 py-2.5 text-sm outline-none placeholder:text-[var(--text-faint)]"
          style="overflow-y: auto;"
          placeholder="输入消息，@ 引用 · / 命令 · # 知识 · 长按空格语音"
          onInput={() => {
            if (ta) setText(ta.value);
            setDraft(activeSessionId(), ta?.value ?? "");
            autogrow();
            triggerCheck.run();
            if (popup()) updatePopupPos(); // 弹层开着时锚点随输入即算，不冻结在打开位置
          }}
          onKeyDown={onKeyDown}
          onKeyUp={(e) => {
            voiceCtl.onSpaceUp(e);
            closePopupIfCaretOut();
          }}
          onClick={closePopupIfCaretOut}
          onBlur={() => setPopup(null)}
          onPaste={onPaste}
          onCompositionEnd={() => (imeLockUntil = Date.now() + 50)}
        />
        <div class="composer-actionbar">
          <AttachMenu onPaths={(paths) => void attachPaths(paths)} />
          <MicControl
            recording={recording}
            activeVoice={activeVoice}
            voiceError={voiceError}
            onToggle={() => voiceCtl.toggle()}
            onEngine={setVoiceEngine}
          />
          <span class={`text-2xs tabular-nums ml-auto ${estimateCls()}`}>~{estimate()} tok</span>
          <ModelPicker />
          <button
            class={sendBtn({ intent: props.streaming() ? "danger" : "primary" })}
            classList={{ "send-ready": !props.streaming() }}
            onClick={() => (props.streaming() ? props.onStop() : sendGuarded())}
            title={props.streaming() ? "停止" : "发送"}
          >
            {props.streaming() ? <Square size={13} /> : <Send size={14} />}
          </button>
        </div>
      </div>
    </div>
  );
}
