import type { ModelOffering } from "../../ts/protocol.generated.ts";

/** A tab's lifecycle state as the tab bar shows it. */
export type TabStatus = "idle" | "running" | "waiting" | "stopped";

/**
 * How much this session asks before it acts (ADR-0020, ADR-0022).
 *
 * The server decides it and the panel renders it: `ask` puts a question behind
 * every guarded action, `yolo` silences the questions the sandbox already
 * contains, and `god` also stops asking about deletions and outward sends while
 * refusing two shapes outright.
 */
export type PermissionMode = "ask" | "yolo" | "god";

/** One open conversation in the sidebar. */
export interface TabState {
  id: string;
  sessionId?: string;
  title: string;
  status: TabStatus;
  unread?: boolean;
}

/** The preview of a write or edit, before the kernel applies it. */
export interface DiffPreview {
  unified: string;
  added: number;
  removed: number;
  truncated: boolean;
  /** True when the kernel will be shown a coarse diff; the counts are approximate. */
  approximate?: boolean;
}

/** One item in a tab's transcript. Plain JSON; no functions or VS Code objects. */
export type UiPart =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string; html: string }
  | { kind: "progress"; text: string }
  | {
      kind: "tool";
      callId: string;
      tool: string;
      actionClass: string;
      paths: string[];
      status: "running" | "ok" | "failed";
      output?: string;
      truncated?: number;
    }
  | {
      kind: "approval";
      requestId: string;
      tool?: string;
      actionClass: string;
      reason: string;
      paths: string[];
      diff?: DiffPreview;
      /**
       * `expired` is a question the kernel is no longer holding: it belongs to a
       * turn whose process is gone, so the card keeps the record and loses its
       * buttons rather than offering an answer nothing is listening for.
       */
      state: "pending" | "allowed" | "denied" | "expired";
    }
  | { kind: "review"; token: string; paths: string[] }
  | { kind: "notice"; level: "info" | "warn" | "error"; text: string };

/** A file or an editor selection the person attached to the next prompt. */
export type Attachment =
  | { id: string; kind: "file"; path: string }
  | {
      id: string;
      kind: "selection";
      path: string;
      startLine: number;
      endLine: number;
      text: string;
    };

/** The workspace file the active editor is focused on, with its 1-based cursor line. */
export interface Focus {
  path: string;
  line: number;
}

/** One workspace file offered by the inline `@` picker. */
export interface FileChoice {
  label: string;
  path: string;
}

/** One session in the history list. */
export interface SessionInfo {
  id: string;
  title: string;
  updated_at: number;
  events: number;
  turns: number;
}

/** Where the extension found the binary, shown in the empty state and output. */
export interface BinaryStatus {
  path: string;
  version: string;
  schema: string;
}

/** Everything the extension host sends into the Webview. */
export type HostMessage =
  | {
      type: "bootstrap";
      tabs: TabState[];
      activeTabId?: string;
      activeTranscript: UiPart[];
      models: ModelOffering[];
      model?: string;
      sessionId?: string;
      binary?: BinaryStatus;
      selection?: Attachment;
      focus?: Focus;
      /** The server's own word on what this session asks, never a local guess. */
      mode?: PermissionMode;
    }
  | { type: "tabs"; tabs: TabState[] }
  | { type: "permissions"; tabId: string; mode: PermissionMode; pending?: boolean }
  | { type: "transcript"; tabId: string; parts: UiPart[] }
  | { type: "delta"; tabId: string; text: string }
  | { type: "part"; tabId: string; part: UiPart }
  | { type: "assistantFinal"; tabId: string; text: string; html: string }
  | { type: "attachments"; tabId: string; items: Attachment[] }
  | { type: "selection"; selection?: Attachment }
  | { type: "focus"; focus?: Focus }
  | { type: "files"; files: FileChoice[] }
  | { type: "sessions"; sessions: SessionInfo[] }
  | { type: "error"; tabId?: string; message: string; detail?: string };

/** Everything the Webview asks the extension host to do. */
export type WebviewMessage =
  | { type: "ready" }
  | { type: "send"; tabId: string; text: string }
  | {
      type: "approval";
      tabId: string;
      requestId: string;
      choice: "once" | "always" | "deny";
    }
  | { type: "cancel"; tabId: string }
  | { type: "newTab" }
  | { type: "selectTab"; tabId: string }
  | { type: "closeTab"; tabId: string }
  | { type: "history" }
  | { type: "openSession"; sessionId: string }
  | { type: "selectModel"; tabId: string; name: string }
  | { type: "context"; tabId: string }
  | { type: "attachFile"; tabId: string; path: string }
  | { type: "removeAttachment"; tabId: string; id: string }
  | { type: "review"; tabId: string; token: string; paths: string[] }
  | { type: "openDiff"; tabId: string; token: string; path: string }
  | { type: "mode"; tabId: string; mode: PermissionMode };

type RecordLike = Record<string, unknown>;

function isRecord(value: unknown): value is RecordLike {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function str(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

/** The protocol's three modes, or nothing when the text is something else. */
export function parseMode(value: unknown): PermissionMode | undefined {
  return value === "ask" || value === "yolo" || value === "god" ? value : undefined;
}

/** Parse one untrusted Webview message; unknown or malformed input is `undefined`. */
export function parseWebviewMessage(raw: unknown): WebviewMessage | undefined {
  if (!isRecord(raw)) return undefined;
  const type = str(raw.type);
  switch (type) {
    case "ready":
      return { type: "ready" };
    case "send": {
      const tabId = str(raw.tabId);
      const text = str(raw.text);
      return tabId === undefined || text === undefined ? undefined : { type, tabId, text };
    }
    case "approval": {
      const tabId = str(raw.tabId);
      const requestId = str(raw.requestId);
      const choice = raw.choice;
      if (tabId === undefined || requestId === undefined) return undefined;
      if (choice !== "once" && choice !== "always" && choice !== "deny") return undefined;
      return { type, tabId, requestId, choice };
    }
    case "cancel":
    case "selectTab":
    case "closeTab":
    case "context": {
      const tabId = str(raw.tabId);
      return tabId === undefined ? undefined : { type, tabId };
    }
    case "attachFile": {
      const tabId = str(raw.tabId);
      const path = str(raw.path);
      return tabId === undefined || path === undefined ? undefined : { type, tabId, path };
    }
    case "newTab":
      return { type };
    case "history":
      return { type };
    case "openSession": {
      const sessionId = str(raw.sessionId);
      return sessionId === undefined ? undefined : { type, sessionId };
    }
    case "selectModel": {
      const tabId = str(raw.tabId);
      const name = str(raw.name);
      return tabId === undefined || name === undefined ? undefined : { type, tabId, name };
    }
    case "removeAttachment": {
      const tabId = str(raw.tabId);
      const id = str(raw.id);
      return tabId === undefined || id === undefined ? undefined : { type, tabId, id };
    }
    case "review": {
      const tabId = str(raw.tabId);
      const token = str(raw.token);
      const paths = raw.paths;
      if (tabId === undefined || token === undefined || !isStringArray(paths)) return undefined;
      return { type, tabId, token, paths };
    }
    case "openDiff": {
      const tabId = str(raw.tabId);
      const token = str(raw.token);
      const path = str(raw.path);
      return tabId === undefined || token === undefined || path === undefined
        ? undefined
        : { type, tabId, token, path };
    }
    case "mode": {
      const tabId = str(raw.tabId);
      const mode = parseMode(raw.mode);
      return tabId === undefined || mode === undefined ? undefined : { type, tabId, mode };
    }
    default:
      return undefined;
  }
}
