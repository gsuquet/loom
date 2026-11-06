export interface DraftComment {
  id: string;
  line: number;
  text: string;
  dropped: boolean;
  by: "Reviewer" | "You";
}

/** Port of the sentinel server of the tests: a request there proves that an attack ran. */
export const SENTINEL = "http://127.0.0.1:47123";

const attack = `window.__pwned = (window.__pwned || 0) + 1;
fetch('${SENTINEL}/fetch-from-script').catch(()=>{});
window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke('loom_api', { method: 'POST', path: '/v1/tasks', body: { title: 'PWNED by MR body', kind: 'chore', context: 'demo', status: 'inbox' } }).catch(()=>{});`;

/** The fake MR body is hostile on purpose: criterion 6 runs on the same screen. */
export const MR = {
  title: "Fake MR: payment retry logic",
  key: "acme/shop!412",
  author: "fake-author",
  body: `## What

Retries a fake payment, with **backoff**. Fake data only.

- first item
- second item with \`code\`

[A normal link](https://example.invalid/docs)

![remote image](${SENTINEL}/remote-image.png)

<script>${attack}</script>

<img src="x" onerror="${attack.replaceAll('"', "&quot;")}">

<img src="${SENTINEL}/raw-html-image.png">

<a href="javascript:window.__pwned=(window.__pwned||0)+1">raw js link</a>

[markdown js link](javascript:window.__pwned=(window.__pwned||0)+1)

<iframe srcdoc="<script>parent.__pwned=(parent.__pwned||0)+1</script>"></iframe>

<style>body { display: none !important; }</style>
`,
};

export const INITIAL_COMMENTS: DraftComment[] = [
  {
    id: "c1",
    line: 8,
    text: "suggestion: name this value after what it holds.",
    dropped: false,
    by: "Reviewer",
  },
  { id: "c2", line: 40, text: "nitpick: this comment is fake.", dropped: false, by: "Reviewer" },
  {
    id: "c3",
    line: 900,
    text: "issue (blocking): fake finding in a later file.",
    dropped: false,
    by: "Reviewer",
  },
];
