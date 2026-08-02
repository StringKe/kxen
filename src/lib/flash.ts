// 统一操作反馈（flash/toast）：全库唯一实现。
// 成功绿/失败红；失败必须带原因（调用方责任，lint 无法强制，评审纪律）。
// 计时器句柄管理：连续消息不抢清（每条独立计时），可手动关闭。
import { createSignal } from "solid-js";

export interface FlashMsg {
  id: number;
  text: string;
  kind: "ok" | "err";
}

export interface Flash {
  msgs: () => FlashMsg[];
  show: (text: string, kind?: "ok" | "err", ttlMs?: number) => void;
  dismiss: (id: number) => void;
}

let seq = 0;

export function createFlash(defaultTtlMs = 4000): Flash {
  const [msgs, setMsgs] = createSignal<FlashMsg[]>([]);
  const timers = new Map<number, ReturnType<typeof setTimeout>>();
  const dismiss = (id: number) => {
    const t = timers.get(id);
    if (t) clearTimeout(t);
    timers.delete(id);
    setMsgs((prev) => prev.filter((m) => m.id !== id));
  };
  const show = (text: string, kind: "ok" | "err" = "ok", ttlMs = defaultTtlMs) => {
    const id = ++seq;
    setMsgs((prev) => [...prev.slice(-2), { id, text, kind }]); // 最多 3 条，防刷屏堆叠
    if (ttlMs > 0)
      timers.set(
        id,
        setTimeout(() => dismiss(id), ttlMs),
      );
  };
  return { msgs, show, dismiss };
}

// 全局单例：任何组件 import 即用，宿主在 App.tsx 挂一次 <FlashHost/>
export const flash = createFlash();

export function flashOk(text: string): void {
  flash.show(text, "ok");
}

export function flashErr(text: string): void {
  flash.show(text, "err", 6000); // 错误多停 2s：用户需要读完原因
}
