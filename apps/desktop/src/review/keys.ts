/** Review keys, from docs/design/user-workflow.md (Review) and the prototype. */
export type Action =
  | "lineDown"
  | "lineUp"
  | "fileNext"
  | "filePrev"
  | "commentNext"
  | "commentPrev"
  | "comment"
  | "edit"
  | "toggleKeep"
  | "viewed"
  | "toggleView"
  | "focusList"
  | "listApplyNew"
  | "upload"
  | "back"
  | "notify"
  | "openSource";

export interface KeyLike {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
}

/**
 * Maps a key event to an action. It reads `key` only, never `code`: on a macOS AZERTY keyboard
 * `a` is the physical `KeyQ` and `m` is the physical `Semicolon`.
 * Option is refused: on macOS it produces other characters, and a shortcut must not need it.
 */
export function resolveKey(e: KeyLike, editing: boolean): Action | null {
  const primary = e.metaKey || e.ctrlKey;
  if (e.key === "Enter" && primary) return "upload";
  if (e.key === "Escape") return "back";
  if (editing || primary || e.altKey) return null;
  switch (e.key) {
    case "j":
      return "lineDown";
    case "k":
      return "lineUp";
    case "n":
      return "fileNext";
    case "p":
      return "filePrev";
    case "m":
      return "commentNext";
    case "M":
      return "commentPrev";
    case "c":
      return "comment";
    case "e":
      return "edit";
    case "x":
      return "toggleKeep";
    case "f":
      return "viewed";
    case "v":
      return "toggleView";
    case "h":
      return "focusList";
    case "y":
      return "listApplyNew";
    case "t":
      return "notify";
    case "o":
      return "openSource";
    default:
      return null;
  }
}
