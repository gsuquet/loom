# 9. Mobile capture via Cloudflare, without GitHub Actions

- Status: Accepted
- Date: 2026-11-09

## Context

Capturing links, text and files from a phone must work offline-first and land in the
knowledge inbox without a laptop. The knowledge repo is private; capture must not require exposing it
or running CI on every capture.

## Decision

An Android PWA (`share_target`) served by Cloudflare Pages behind Cloudflare Access posts
to a Worker. The Worker snapshots the source to R2, converts it to markdown and commits it
to `inbox/` of the knowledge repo through the GitHub Contents API. The PWA queues captures while offline.
`loom ingest` later performs clean extraction locally. No GitHub Actions are involved.

## Consequences

- Capture works without the laptop and without a self-hosted server.
- One Cloudflare deployment and one fine-grained GitHub token (stored as a Worker secret)
  to maintain.
- Capture targets the knowledge repo only; isolated-context data is never captured from the phone.
- Detailed architecture is deferred to the slice 2 design.
