// TextComposer 粘贴/附件/草稿标注/token 估算实测（350 行门禁从 text-composer.test.tsx 拆出）：
// 图片 chip 释放 images、混合剪贴板文本不丢、小粘贴 CRLF 归一、截断标注剥除、估算分级随 ctx 窗。
import { createSignal } from "solid-js";
import { render } from "solid-js/web";
import "../../styles.css";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { userEvent } from "@vitest/browser/context";
import TextComposer from "./TextComposer";
import { setActiveSessionId } from "../../lib/state";
import { clearDraft, getDraft, setDraft } from "../../lib/drafts";

const imageMock = vi.hoisted(() => ({ encode: vi.fn() }));

vi.mock("./image-scale", () => ({ fileToImageDataUrl: imageMock.encode }));

vi.mock("../../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../../lib/chat")>();
  return {
    ...orig,
    commandList: async () => [],
    // token 估算分级数据源：固定当前模型，配合下方 catalog 的 ctx=100
    currentModel: async () => ({ provider: "xai", model: "grok-1" }),
    sessionList: async () => [],
  };
});

vi.mock("../../lib/models", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../../lib/models")>();
  return {
    ...orig,
    modelsCatalog: async () => [
      {
        provider: "xai",
        provider_name: "xAI",
        fetched_at: 0,
        source: "test",
        models: [
          {
            id: "grok-1",
            name: "Grok 1",
            family: "grok",
            reasoning: false,
            tool_call: true,
            attachment: false,
            modalities_in: ["text"],
            context: 100,
            output: 4096,
          },
        ],
      },
    ],
  };
});

beforeEach(() => {
  imageMock.encode.mockReset().mockResolvedValue("data:image/png;base64,WA==");
});

afterEach(() => {
  clearDraft("");
  clearDraft("s9");
  clearDraft("s1");
  clearDraft("s2");
  localStorage.removeItem("kxen:draft:s9");
  setActiveSessionId("");
  // 失败用例没跑到 dispose 时清场：残留 composer 会让下一个用例的 ta() 抓到旧 textarea
  document.body.innerHTML = "";
});

function mount(
  onSend: (
    text: string,
    images?: Array<unknown>,
  ) =>
    | boolean
    | void
    | { admitted: boolean; sessionId: string }
    | Promise<boolean | void | { admitted: boolean; sessionId: string }> = () => {},
) {
  const [tick, setTick] = createSignal(0);
  const dispose = render(
    () => (
      <TextComposer
        streaming={() => false}
        onSend={(t, _c, imgs) => onSend(t, imgs)}
        onStop={() => {}}
        focusTick={tick}
      />
    ),
    document.body,
  );
  return { dispose, setTick, ta: () => document.querySelector<HTMLTextAreaElement>("textarea")! };
}

function pasteFile(ta: HTMLTextAreaElement, dt: DataTransfer) {
  ta.dispatchEvent(
    new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }),
  );
}

describe("TextComposer 粘贴/附件 (webkit)", () => {
  it("图片编码未决时立即重复 Enter：等待附件后只发送一次完整图片载荷", async () => {
    let resolveImage!: (value: string) => void;
    imageMock.encode.mockReturnValue(
      new Promise((resolve) => {
        resolveImage = resolve;
      }),
    );
    const sent = vi.fn();
    setActiveSessionId("s1");
    const { dispose, ta } = mount((text, images) => sent(text, images));
    await new Promise((resolve) => setTimeout(resolve, 100));
    const data = new DataTransfer();
    data.items.add(new File(["x"], "late.png", { type: "image/png" }));
    pasteFile(ta(), data);
    ta().value = "附图";
    ta().dispatchEvent(new InputEvent("input", { bubbles: true }));
    const enter = () =>
      ta().dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
      );
    enter();
    enter();
    await Promise.resolve();
    expect(sent).not.toHaveBeenCalled();

    resolveImage("data:image/png;base64,TEFURQ==");
    await vi.waitFor(() => expect(sent).toHaveBeenCalledOnce());
    expect(sent).toHaveBeenCalledWith("附图", [{ media_type: "image/png", data: "TEFURQ==" }]);
    expect(ta().value).toBe("");
    dispose();
  });

  it("图片 chip 移除后发送不再携带图片数据（images 随 chip 释放）", async () => {
    let imgs: unknown[] = [];
    const { dispose, ta } = mount((_t, i) => void (imgs = i ?? []));
    await new Promise((r) => setTimeout(r, 100));
    const dt = new DataTransfer();
    dt.items.add(new File(["x"], "a.png", { type: "image/png" }));
    pasteFile(ta(), dt);
    await vi.waitFor(() =>
      expect(document.querySelector(".composer-card")?.textContent).toContain("图片 png"),
    );
    const chipX = [...document.querySelectorAll<HTMLElement>(".composer-card button")].find((b) =>
      b.parentElement?.textContent?.includes("图片 png"),
    )!;
    chipX.click();
    await new Promise((r) => setTimeout(r, 30));
    expect(document.querySelector(".composer-card")?.textContent).not.toContain("图片 png");
    ta().focus();
    await userEvent.keyboard("hi{Enter}");
    expect(imgs.length).toBe(0);
    dispose();
  });

  it("会话准入失败：图片 chip 与文本一起恢复，不丢附件", async () => {
    let rejectAdmission!: () => void;
    const admission = new Promise<boolean>((resolve) => {
      rejectAdmission = () => resolve(false);
    });
    setActiveSessionId("s9");
    const { dispose, ta } = mount(() => admission);
    await new Promise((r) => setTimeout(r, 100));
    const dt = new DataTransfer();
    dt.items.add(new File(["x"], "a.png", { type: "image/png" }));
    pasteFile(ta(), dt);
    await vi.waitFor(() =>
      expect(document.querySelector(".composer-card")?.textContent).toContain("图片 png"),
    );
    ta().focus();
    await userEvent.keyboard("附图说明{Enter}");
    expect(ta().value).toBe("");
    await userEvent.keyboard("，继续补充");
    rejectAdmission();
    await vi.waitFor(() => expect(ta().value).toBe("附图说明\n，继续补充"));
    expect(getDraft("s9")).toBe("附图说明\n，继续补充");
    expect(document.querySelector(".composer-card")?.textContent).toContain("图片 png");
    dispose();
  });

  it("准入期间切会话：旧文本和图片只恢复到原会话", async () => {
    let rejectAdmission!: () => void;
    const admission = new Promise<{ admitted: boolean; sessionId: string }>((resolve) => {
      rejectAdmission = () => resolve({ admitted: false, sessionId: "s1" });
    });
    setActiveSessionId("s1");
    const { dispose, setTick, ta } = mount(() => admission);
    await new Promise((resolve) => setTimeout(resolve, 100));
    const data = new DataTransfer();
    data.items.add(new File(["x"], "a.png", { type: "image/png" }));
    pasteFile(ta(), data);
    await vi.waitFor(() =>
      expect(document.querySelector(".composer-card")?.textContent).toContain("图片 png"),
    );
    ta().focus();
    await userEvent.keyboard("旧会话内容{Enter}");
    setActiveSessionId("s2");
    setTick(1);
    await new Promise((resolve) => setTimeout(resolve, 50));
    // 同一旧会话可由另一个 Composer/窗口继续产生草稿；失败恢复必须保留清晰消息边界。
    setDraft("s1", "旧会话在途新输入");
    rejectAdmission();
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(ta().value).toBe("");
    expect(document.querySelector(".composer-card")?.textContent).not.toContain("图片 png");
    expect(getDraft("s1")).toBe("旧会话内容\n旧会话在途新输入");
    setActiveSessionId("s1");
    setTick(2);
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(ta().value).toBe("旧会话内容\n旧会话在途新输入");
    expect(document.querySelector(".composer-card")?.textContent).toContain("图片 png");
    dispose();
  });

  it("旧 Composer 卸载后准入才失败：同会话新实例立即恢复附件", async () => {
    let rejectAdmission!: () => void;
    const admission = new Promise<boolean>((resolve) => {
      rejectAdmission = () => resolve(false);
    });
    setActiveSessionId("s1");
    const old = mount(() => admission);
    await new Promise((resolve) => setTimeout(resolve, 100));
    const data = new DataTransfer();
    data.items.add(new File(["x"], "a.png", { type: "image/png" }));
    pasteFile(old.ta(), data);
    await vi.waitFor(() =>
      expect(document.querySelector(".composer-card")?.textContent).toContain("图片 png"),
    );
    old.ta().focus();
    await userEvent.keyboard("卸载后恢复{Enter}");
    old.dispose();
    const current = mount();
    await new Promise((resolve) => setTimeout(resolve, 50));
    rejectAdmission();
    await vi.waitFor(() => expect(current.ta().value).toBe("卸载后恢复"));
    expect(document.querySelector(".composer-card")?.textContent).toContain("图片 png");
    current.dispose();
  });

  it("混合剪贴板（图片+文本）：文本随附件一起上屏，不被 files 吞掉", async () => {
    const { dispose, ta } = mount();
    await new Promise((r) => setTimeout(r, 100));
    const dt = new DataTransfer();
    dt.items.add(new File(["x"], "a.png", { type: "image/png" }));
    dt.setData("text/plain", "附图说明");
    pasteFile(ta(), dt);
    expect(ta().value).toBe("附图说明");
    await vi.waitFor(() =>
      expect(document.querySelector(".composer-card")?.textContent).toContain("图片 png"),
    );
    dispose();
  });

  it("小粘贴 CRLF 归一为 LF", async () => {
    const { dispose, ta } = mount();
    await new Promise((r) => setTimeout(r, 100));
    const dt = new DataTransfer();
    dt.setData("text/plain", "a\r\nb");
    pasteFile(ta(), dt);
    await new Promise((r) => setTimeout(r, 50));
    expect(ta().value).toBe("a\nb");
    dispose();
  });

  it("冷启动恢复的截断草稿剥掉标注（标注是存储层告示，发出即污染 prompt）", async () => {
    localStorage.setItem("kxen:draft:s9", "半截草稿\n[草稿过长，已截断]");
    const { dispose, setTick, ta } = mount();
    await new Promise((r) => setTimeout(r, 100));
    setActiveSessionId("s9");
    setTick(1);
    await new Promise((r) => setTimeout(r, 100));
    expect(ta().value).toBe("半截草稿");
    dispose();
  });

  it("token 估算分级跟当前模型 ctx 窗（mock ctx=100：80 警 / 95 险）", async () => {
    const { dispose, ta } = mount();
    await new Promise((r) => setTimeout(r, 150));
    const span = () => document.querySelector<HTMLElement>(".tabular-nums")!;
    const type = (v: string) => {
      const el = ta();
      el.value = v;
      el.dispatchEvent(new InputEvent("input", { bubbles: true }));
    };
    type("x".repeat(340)); // 85 tok > 80（窗的 80%）
    await new Promise((r) => setTimeout(r, 30));
    expect(span().className).toContain("--warn");
    type("x".repeat(420)); // 105 tok > 95（窗的 95%）
    await new Promise((r) => setTimeout(r, 30));
    expect(span().className).toContain("--err");
    dispose();
  });
});
