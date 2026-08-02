import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CodingRulesInfo } from "../../lib/knowledge";

const h = vi.hoisted(() => ({
  get: vi.fn(),
  set: vi.fn(),
}));

vi.mock("../../lib/knowledge", () => ({
  codingRulesGet: h.get,
  codingRulesSet: h.set,
}));

import CodingRulesBlock from "./CodingRulesBlock";
import { flash } from "../../lib/flash";

const rules: CodingRulesInfo = {
  enabled: true,
  content: "只写 WHY",
};

beforeEach(() => {
  h.get.mockReset();
  h.set.mockReset();
  h.get.mockResolvedValue(rules);
  h.set.mockResolvedValue(undefined);
});

afterEach(() => {
  document.body.innerHTML = "";
  for (const message of flash.msgs()) flash.dismiss(message.id);
});

describe("CodingRulesBlock", () => {
  it("loads, expands, disables, and enables built-in rules", async () => {
    const dispose = render(() => <CodingRulesBlock />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("内置编码规则"));

    const disclosure = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent?.includes("全文"),
    );
    disclosure?.click();
    expect(document.body.textContent).toContain("只写 WHY");
    disclosure?.click();
    expect(document.body.textContent).not.toContain("只写 WHY");

    const toggle = document.body.querySelector<HTMLButtonElement>("button[title^='停用']");
    toggle?.click();
    await vi.waitFor(() => expect(h.set).toHaveBeenCalledWith(false));
    expect(document.body.querySelector("button[title='启用']")).toBeTruthy();

    document.body.querySelector<HTMLButtonElement>("button[title='启用']")?.click();
    await vi.waitFor(() => expect(h.set).toHaveBeenCalledWith(true));
    dispose();
  });

  it("load failure shows UNKNOWN with retry, and toggle failure preserves state", async () => {
    h.get.mockRejectedValueOnce(new Error("unavailable"));
    const dispose = render(() => <CodingRulesBlock />, document.body);
    await vi.waitFor(() =>
      expect(document.body.textContent).toContain(
        "内置编码规则读取失败，状态 UNKNOWN：unavailable",
      ),
    );
    const retry = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "重试",
    );
    retry?.click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("内置编码规则"));

    h.set.mockRejectedValueOnce("write failed");
    document.body.querySelector<HTMLButtonElement>("button[title^='停用']")?.click();
    await vi.waitFor(() =>
      expect(flash.msgs().some((message) => message.text.includes("write failed"))).toBe(true),
    );
    expect(document.body.querySelector("button[title^='停用']")).toBeTruthy();
    dispose();
  });
});
