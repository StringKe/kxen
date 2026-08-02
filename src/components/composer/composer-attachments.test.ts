// 附件装配失败可见性：授权/读取/图片编码失败都落 err chip（title 带原因），不再静默跳过。
import { afterEach, describe, expect, it, vi } from "vitest";
import { createAttachments } from "./composer-attachments";
import { fileToImageDataUrl } from "./image-scale";
import type { RowChip } from "./RowChips";

const rpcMock = vi.hoisted(() => ({
  impl: (_method: string, _params?: unknown) =>
    Promise.reject(new Error("unexpected call")) as Promise<unknown>,
}));
const stateMock = vi.hoisted(() => ({
  ensure: vi.fn(async () => "s1"),
  flashErr: vi.fn(),
}));
vi.mock("../../lib/client", () => ({
  client: { rpc: (method: string, params?: unknown) => rpcMock.impl(method, params) },
}));
vi.mock("../../lib/state", () => ({
  ensureActiveSession: stateMock.ensure,
}));
vi.mock("../../lib/flash", () => ({ flashErr: stateMock.flashErr }));
vi.mock("./image-scale", () => ({
  fileToImageDataUrl: vi.fn(),
}));

function harness() {
  const chips: Omit<RowChip, "id">[] = [];
  const images = new Map<string, { media_type: string; data: string }>();
  const api = createAttachments({ images, pushChip: (c) => chips.push(c) });
  return { chips, images, ...api };
}

afterEach(() => {
  rpcMock.impl = () => Promise.reject(new Error("unexpected call"));
  stateMock.ensure.mockReset().mockResolvedValue("s1");
  stateMock.flashErr.mockClear();
  vi.mocked(fileToImageDataUrl).mockReset();
});

describe("attachPaths 会话创建失败", () => {
  it("ensureActiveSession 失败：flashErr 上屏，不出 chip、不浮 unhandled rejection", async () => {
    stateMock.ensure.mockRejectedValueOnce(new Error("create boom"));
    const { chips, attachPaths } = harness();
    await attachPaths(["/w/a.txt"]);
    expect(stateMock.flashErr).toHaveBeenCalledTimes(1);
    expect(String(stateMock.flashErr.mock.calls[0]?.[0])).toContain("添加附件失败");
    expect(String(stateMock.flashErr.mock.calls[0]?.[0])).toContain("create boom");
    expect(chips.length).toBe(0);
  });
});

describe("attachPaths 失败 err chip", () => {
  it("授权失败 push err chip：label 是 basename，title 带后端原因", async () => {
    rpcMock.impl = () => Promise.reject(new Error("path not allowed"));
    const { chips, attachPaths } = harness();
    await attachPaths(["/etc/secret.png"]);
    expect(chips.length).toBe(1);
    expect(chips[0]?.kind).toBe("err");
    expect(chips[0]?.label).toBe("secret.png");
    expect(chips[0]?.title).toContain("授权失败");
    expect(chips[0]?.title).toContain("path not allowed");
  });

  it("混合路径：成功的照常成 chip，失败的成 err chip（互不吞）", async () => {
    rpcMock.impl = (method: string, params?: unknown) => {
      const p = params as { path: string };
      if (method === "fs.allow_path" && p.path === "/w/ok.txt") {
        return Promise.resolve({ path: "/w/ok.txt", rel: "ok.txt" });
      }
      return Promise.reject(new Error("denied"));
    };
    const { chips, attachPaths } = harness();
    await attachPaths(["/w/ok.txt", "/bad.txt"]);
    expect(chips.map((c) => c.kind)).toEqual(["file", "err"]);
    expect(chips[0]?.ref).toBe("ok.txt");
    expect(chips[1]?.label).toBe("bad.txt");
  });
});

describe("attachFiles 图片失败 err chip", () => {
  it("编码/读取失败 push err chip（不再静默）", async () => {
    vi.mocked(fileToImageDataUrl).mockRejectedValue(new Error("decode boom"));
    const { chips, attachFiles } = harness();
    attachFiles([new File([new Uint8Array([1])], "shot.png", { type: "image/png" })]);
    await vi.waitFor(() => expect(chips.length).toBe(1));
    expect(chips[0]?.kind).toBe("err");
    expect(chips[0]?.label).toBe("shot.png");
    expect(chips[0]?.title).toContain("图片读取失败");
    expect(chips[0]?.title).toContain("decode boom");
  });

  it("成功路径不受影响：image chip + images map 入库", async () => {
    vi.mocked(fileToImageDataUrl).mockResolvedValue("data:image/png;base64,QUJD");
    const { chips, images, attachFiles } = harness();
    attachFiles([new File([new Uint8Array([1])], "shot.png", { type: "image/png" })]);
    await vi.waitFor(() => expect(chips.length).toBe(1));
    expect(chips[0]?.kind).toBe("image");
    expect(images.get("data:image/png;base64,QUJD")).toEqual({
      media_type: "image/png",
      data: "QUJD",
    });
  });
});
