import MarkdownIt from "markdown-it";
import type { ReactNode } from "react";
import { createElement, useEffect, useRef } from "react";

// html: false turns raw HTML into text tokens. Rendering walks tokens to React elements, so no
// string ever reaches innerHTML. markdown-it also refuses javascript: links by default.
const md = new MarkdownIt({ html: false, linkify: false });

type Token = ReturnType<typeof md.parse>[number];

const ALLOWED_TAGS = new Set([
  "p",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "ul",
  "ol",
  "li",
  "strong",
  "em",
  "s",
  "code",
  "pre",
  "blockquote",
  "hr",
  "br",
  "table",
  "thead",
  "tbody",
  "tr",
  "th",
  "td",
  "a",
]);

function isSafeHref(href: string): boolean {
  return /^(https?:|mailto:|#)/i.test(href);
}

function renderTokens(tokens: Token[]): ReactNode[] {
  const out: ReactNode[] = [];
  const stack: { tag: string; attrs: Record<string, string>; children: ReactNode[] }[] = [];
  const sink = () => stack.at(-1)?.children ?? out;
  let n = 0;
  const walk = (list: Token[]) => {
    for (const t of list) {
      n += 1;
      const key = String(n);
      if (t.type === "inline") walk(t.children ?? []);
      else if (t.type === "text" || t.type === "html_inline" || t.type === "html_block")
        sink().push(t.content);
      else if (t.type === "softbreak") sink().push(" ");
      else if (t.type === "code_inline") sink().push(createElement("code", { key }, t.content));
      else if (t.type === "fence" || t.type === "code_block")
        sink().push(createElement("pre", { key }, createElement("code", null, t.content)));
      else if (t.type === "image") {
        // A remote image is a link: no request leaves until the reader chooses to.
        const src = String(t.attrGet("src") ?? "");
        const label = t.content || src;
        sink().push(
          isSafeHref(src)
            ? createElement(
                "a",
                { key, href: src, rel: "noreferrer", "data-remote-image": "true" },
                `Image: ${label}`,
              )
            : `Image: ${label}`,
        );
      } else if (t.nesting === 1) {
        const attrs: Record<string, string> = {};
        const href = String(t.attrGet("href") ?? "");
        if (t.tag === "a") {
          if (href && isSafeHref(href)) attrs.href = href;
          attrs.rel = "noreferrer";
        }
        stack.push({ tag: t.tag, attrs, children: [] });
      } else if (t.nesting === -1) {
        const top = stack.pop();
        if (top) {
          const tag = ALLOWED_TAGS.has(top.tag) ? top.tag : "div";
          sink().push(createElement(tag, { key, ...top.attrs }, ...top.children));
        }
      } else if (t.tag && ALLOWED_TAGS.has(t.tag)) {
        sink().push(createElement(t.tag, { key }));
      }
    }
  };
  walk(tokens);
  return out;
}

export function Markdown({ source }: { source: string }) {
  return <div className="md">{renderTokens(md.parse(source, {}))}</div>;
}

// The negative control of criterion 6. The constant is false in a normal build and the bundler
// drops this branch: a test greps the normal bundle for it.
function UnsafeMarkdown({ source }: { source: string }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!ref.current) return;
    const unsafe = new MarkdownIt({ html: true, linkify: false });
    // createContextualFragment, unlike innerHTML, runs <script> elements.
    ref.current.replaceChildren(
      document.createRange().createContextualFragment(unsafe.render(source)),
    );
  }, [source]);
  return <div className="md" ref={ref} data-unsafe-renderer="SPIKE_UNSAFE_RENDERER" />;
}

export const BodyRenderer = import.meta.env.VITE_SPIKE_UNSAFE === "1" ? UnsafeMarkdown : Markdown;
