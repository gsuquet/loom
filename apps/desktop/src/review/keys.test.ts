import { describe, expect, it } from "vitest";
import { resolveKey } from "./keys";

const k = (key: string, extra = {}) => ({
  key,
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  ...extra,
});

describe("resolveKey", () => {
  it("uses the produced letter, whatever the physical key (macOS AZERTY)", () => {
    // AZERTY produces "a" on KeyQ and "m" on Semicolon; `code` must not matter.
    const azertyM = { ...k("m"), code: "Semicolon" };
    expect(resolveKey(azertyM as never, false)).toBe("commentNext");
    // QWERTY KeyM on AZERTY produces ",": it must do nothing, not comment-next.
    expect(resolveKey({ ...k(","), code: "KeyM" } as never, false)).toBeNull();
  });
  it("keeps upper case apart", () => {
    expect(resolveKey(k("M"), false)).toBe("commentPrev");
    expect(resolveKey(k("m"), false)).toBe("commentNext");
  });
  it("needs no digit, punctuation or Option key", () => {
    for (const key of "jknpmMcexfvhyt") expect(resolveKey(k(key), false)).not.toBeNull();
    expect(resolveKey(k("j", { altKey: true }), false)).toBeNull();
  });
  it("lets a text field keep its letters, but not Esc and Cmd+Enter", () => {
    expect(resolveKey(k("j"), true)).toBeNull();
    expect(resolveKey(k("Escape"), true)).toBe("back");
    expect(resolveKey(k("Enter", { metaKey: true }), true)).toBe("upload");
  });
  it("leaves Cmd+letter to the system", () => {
    expect(resolveKey(k("c", { metaKey: true }), false)).toBeNull();
  });
});
