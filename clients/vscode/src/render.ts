import { EVENTS } from "../../ts/protocol.generated.ts";
import type {
  ApprovalDecided,
  ApprovalRequested,
  EventEnvelope,
  JsonValue,
  MessageAppended,
  PermissionsRelaxed,
  PolicyDecided,
  ToolCall,
  ToolResult,
} from "../../ts/protocol.generated.ts";

/** What a surface can show without knowing the protocol. */
export type UiPart =
  | { kind: "markdown"; text: string }
  | { kind: "progress"; text: string };

/** How much of one tool's output reaches the chat before it is cut. */
export const MAX_TOOL_OUTPUT_CHARS = 4000;

/**
 * Absolute paths that appear as string values in a tool call's arguments.
 *
 * The arguments are opaque JSON, so this is a harvest and not a parse: a tool
 * that names a file in a way this misses simply contributes one less file to
 * the review list. Nothing here touches the filesystem.
 */
export function pathsInArgs(args: JsonValue): string[] {
  const found: string[] = [];
  const visit = (value: JsonValue): void => {
    if (typeof value === "string") {
      if (looksLikeAPath(value)) found.push(value);
      return;
    }
    if (Array.isArray(value)) {
      for (const item of value) visit(item);
      return;
    }
    if (value !== null && typeof value === "object") {
      for (const item of Object.values(value)) visit(item);
    }
  };
  visit(args);
  return [...new Set(found)];
}

function looksLikeAPath(value: string): boolean {
  if (value.includes("://")) return false;
  if (value.startsWith("/")) return value.length > 1;
  return /^[A-Za-z]:[\\/]/.test(value);
}

/**
 * Turns the kernel's events into things a chat can show.
 *
 * Two rules hold everywhere in here: a value that came from an event is quoted
 * exactly (a rule name, a scope, a verdict — this layer never has an opinion
 * about any of them), and nothing is shown twice.
 */
export class Renderer {
  private readonly cwd: string;
  private readonly files: string[] = [];
  private streamed = false;

  constructor(options: { cwd: string }) {
    this.cwd = options.cwd;
  }

  /** A new turn: the file list and the "already streamed" flag start over. */
  beginTurn(): void {
    this.files.length = 0;
    this.streamed = false;
  }

  /** A fragment of the answer. Verbatim: a chunk can end mid-word. */
  delta(text: string): UiPart[] {
    this.streamed = true;
    return [{ kind: "markdown", text }];
  }

  event(envelope: EventEnvelope): UiPart[] {
    switch (envelope.kind) {
      case EVENTS.toolCall:
        return this.toolCall(envelope.data as unknown as ToolCall);
      case EVENTS.toolResult:
        return this.toolResult(envelope.data as unknown as ToolResult);
      case EVENTS.policyDecided: {
        const decided = envelope.data as unknown as PolicyDecided;
        return [
          {
            kind: "markdown",
            text: `policy: ${decided.verdict} by ${decided.rule} (${decided.action_class})`,
          },
        ];
      }
      case EVENTS.approvalRequested: {
        const asked = envelope.data as unknown as ApprovalRequested;
        return [
          { kind: "markdown", text: `approval needed: ${asked.action_class} — ${asked.reason}` },
        ];
      }
      case EVENTS.approvalDecided: {
        const decided = envelope.data as unknown as ApprovalDecided;
        const outcome = decided.granted ? "granted" : "denied";
        return [{ kind: "markdown", text: `approval ${outcome} (${decided.scope})` }];
      }
      case EVENTS.permissionsRelaxed: {
        const relaxed = envelope.data as unknown as PermissionsRelaxed;
        // ADR-0007 §5: a session that stops asking is a session whose log has to
        // say why, and a surface that hides it would be hiding the relaxation.
        const state = relaxed.auto_approve
          ? "approval prompts are off for this session (the sandbox is unchanged)"
          : "approval prompts are on again for this session";
        return [{ kind: "markdown", text: `${state} — ${relaxed.reason}` }];
      }
      case EVENTS.messageAppended:
        return this.message(envelope.data as unknown as MessageAppended);
      default:
        return [];
    }
  }

  /**
   * What a reopened session shows: where you were, not every word ever said.
   *
   * The chat's transcript belongs to the window; the session belongs to the log
   * (spec §4.6). So this is a card, not a replay.
   */
  resumed(events: EventEnvelope[]): UiPart[] {
    if (events.length === 0) {
      return [{ kind: "markdown", text: "Resumed session — the log has no events yet." }];
    }
    const newest = (role: string): string | undefined => {
      for (let index = events.length - 1; index >= 0; index--) {
        const envelope = events[index];
        if (envelope.kind !== EVENTS.messageAppended) continue;
        const message = envelope.data as unknown as MessageAppended;
        if (message.role === role) return message.text;
      }
      return undefined;
    };
    const lines = [`Resumed session — ${events.length} events in the log.`];
    const asked = newest("user");
    const answered = newest("assistant");
    if (asked !== undefined) lines.push(`\n**You last said:** ${asked}`);
    if (answered !== undefined) lines.push(`\n**The agent last said:** ${answered}`);
    return [{ kind: "markdown", text: lines.join("\n") }];
  }

  /**
   * Every path this turn mentioned.
   *
   * "Mentioned", not "modified": a read mentions a path too. The caller that
   * opens diffs is responsible for showing only the ones that differ.
   */
  touchedFiles(): string[] {
    return [...this.files];
  }

  private toolCall(call: ToolCall): UiPart[] {
    const paths = pathsInArgs(call.args);
    for (const path of paths) {
      if (!this.files.includes(path)) this.files.push(path);
    }
    const where = paths.length === 0 ? "" : ` — ${paths.map((path) => this.display(path)).join(", ")}`;
    return [{ kind: "progress", text: `${call.tool} (${call.action_class})${where}` }];
  }

  private toolResult(result: ToolResult): UiPart[] {
    const output = result.output ?? "";
    const outcome = result.ok ? "" : "failed — ";
    if (output.length <= MAX_TOOL_OUTPUT_CHARS) {
      return [{ kind: "markdown", text: `\n~~~\n${outcome}${output}\n~~~\n` }];
    }
    const dropped = output.length - MAX_TOOL_OUTPUT_CHARS;
    return [
      {
        kind: "markdown",
        text:
          `\n~~~\n${outcome}${output.slice(0, MAX_TOOL_OUTPUT_CHARS)}\n~~~\n` +
          `… truncated ${dropped} of ${output.length} characters\n`,
      },
    ];
  }

  private message(message: MessageAppended): UiPart[] {
    // The person's own words are already in the chat: repeating them would be
    // the surface narrating what just happened over itself.
    if (message.role !== "assistant") return [];
    // The streamed fragments were the same text; the event is the record of it.
    if (this.streamed) return [];
    return [{ kind: "markdown", text: message.text }];
  }

  /** Relative inside the workspace, absolute outside: "outside" has to stay legible. */
  private display(path: string): string {
    const root = this.cwd.endsWith("/") ? this.cwd : `${this.cwd}/`;
    return path.startsWith(root) ? path.slice(root.length) : path;
  }
}
