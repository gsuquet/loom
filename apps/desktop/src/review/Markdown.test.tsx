import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { MR } from "./fixtures";
import { Markdown } from "./Markdown";

const html = renderToStaticMarkup(<Markdown source={MR.body} />);

describe("Markdown with an untrusted body", () => {
  it("creates no element from raw HTML", () => {
    expect(html).not.toMatch(/<(script|img|iframe|style)\b/);
    expect(html).not.toMatch(/<[^>]*\son[a-z]+=/);
  });
  it("shows raw HTML as text", () => {
    expect(html).toContain("&lt;script&gt;");
  });
  it("makes a remote image a link", () => {
    expect(html).toContain('data-remote-image="true"');
    expect(html).toContain("Image: remote image");
  });
  it("creates no javascript: link", () => {
    expect(html).not.toMatch(/href="javascript:/i);
  });
  it("still renders normal Markdown", () => {
    expect(html).toContain("<strong>backoff</strong>");
    expect(html).toContain('href="https://example.invalid/docs"');
  });
});
