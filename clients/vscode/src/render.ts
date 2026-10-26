import { EVENTS } from "../../ts/protocol.generated.ts";
import { renderMarkdown } from "./markdown.ts";
import type {
  ApprovalDecided,
  ApprovalRequested,
  ContextExplain,
  EventEnvelope,
  JsonValue,
  MessageAppended,
  ModelList,
  PermissionsRelaxed,
  PolicyDecided,
  ToolCall,
  ToolResult,
  TurnInterrupted,
} from "../../ts/protocol.generated.ts";
import type { UiPart } from "./webviewProtocol.ts";

export type { UiPart } from "./webviewProtocol.ts";

/** How much of one tool's output reaches the chat before it is cut. */
export const MAX_TOOL_OUTPUT_CHARS = 4000;

function isAbsolutePath(value: string): boolean {
  return value.startsWith("/") || value.startsWith("\\\\") || /^[A-Za-z]:[\\/]/.test(value);
}

function joinWorkspace(cwd: string, value: string): string {
  const separator = cwd.includes("\\") && !cwd.includes("/") ? "\\" : "/";
  return `${cwd.replace(/[\\/]+$/, "")}${separator}${value.replace(/^[.][\\/]/, "")}`;
}

/**
 * Paths in a tool call's arguments.
 *
 * An absolute-looking string is kept. A relative string under the `path` key is
 * resolved against the workspace, because the kernel resolves it that way too;
 * flags and URLs are ignored. This is still a harvest, not a parse: missing a
 * path degrades the review list, it never invents one.
 */
export function pathsInArgs(args: JsonValue, cwd: string): string[] {
  const found: string[] = [];
  const visit = (value: JsonValue, key: string | undefined): void => {
    if (typeof value === "string") {
      if (value.includes("://")) return;
      if (isAbsolutePath(value)) {
        if (value.length > 1) found.push(value);
        return;
      }
      if (key === "path" && value !== "" && !value.startsWith("-")) {
        found.push(joinWorkspace(cwd, value));
      }
      return;
    }
    if (Array.isArray(value)) {
      for (const item of value) visit(item, key);
      return;
    }
    if (value !== null && typeof value === "object") {
      for (const [childKey, item] of Object.entries(value)) visit(item, childKey);
    }
  };
  visit(args, undefined);
  return [...new Set(found)];
}

/**
 * Turns the kernel's events into things the sidebar can show.
 *
 * Two rules hold everywhere: a value that came from an event is quoted exactly
 * (a rule, a scope, a verdict — this layer has no opinion about any of them),
 * and nothing is shown twice. The host adds a diff to an approval card; the
 * renderer never invents one.
 */
export class Renderer {
  private readonly cwd: string;
  private readonly files: string[] = [];
  private readonly calls = new Map<string, { tool: string; actionClass: string }>();

  constructor(options: { cwd: string }) {
    this.cwd = options.cwd;
  }

  /** A new turn: the file list and the known tool calls start over. */
  beginTurn(): void {
    this.files.length = 0;
    this.calls.clear();
  }

  event(envelope: EventEnvelope): UiPart[] {
    switch (envelope.kind) {
      case EVENTS.toolCall:
        return this.toolCall(envelope.data as unknown as ToolCall);
      case EVENTS.toolResult:
        return this.toolResult(envelope.data as unknown as ToolResult);
      case EVENTS.policyDecided: {
        const decided = envelope.data as unknown as PolicyDecided;
        return [{
          kind: "notice",
          level: "info",
          text: `policy: ${decided.verdict} by ${decided.rule} (${decided.action_class})`,
        }];
      }
      case EVENTS.approvalRequested: {
        const asked = envelope.data as unknown as ApprovalRequested;
        return [{
          kind: "approval",
          requestId: asked.request_id,
          actionClass: asked.action_class,
          reason: asked.reason,
          paths: [],
          state: "pending",
        }];
      }
      case EVENTS.approvalDecided: {
        const decided = envelope.data as unknown as ApprovalDecided;
        const outcome = decided.granted ? "granted" : "denied";
        return [{ kind: "notice", level: "info", text: `approval ${outcome} (${decided.scope})` }];
      }
      case EVENTS.permissionsRelaxed: {
        const relaxed = envelope.data as unknown as PermissionsRelaxed;
        const state = relaxed.auto_approve
          ? "approval prompts are off for this session (the sandbox is unchanged)"
          : "approval prompts are on again for this session";
        return [{ kind: "notice", level: relaxed.auto_approve ? "warn" : "info",
          text: `${state} — ${relaxed.reason}` }];
      }
      case EVENTS.turnInterrupted: {
        const interrupted = envelope.data as unknown as TurnInterrupted;
        return [{ kind: "notice", level: "warn", text: `interrupted — ${interrupted.reason}` }];
      }
      case EVENTS.messageAppended:
        return this.message(envelope.data as unknown as MessageAppended);
      default:
        return [];
    }
  }

  /** The models this session offers, with the current one marked as given. */
  models(list: ModelList): UiPart[] {
    const lines = list.models.map((offering) => {
      const current = offering.name === list.selected ? " — current" : "";
      return `- \`${offering.name}\` (${offering.model})${current}`;
    });
    return [{ kind: "notice", level: "info", text: `Models for this session:\n${lines.join("\n")}` }];
  }

  /** What the next request will carry, exactly as the kernel reported it. */
  context(explain: ContextExplain): UiPart[] {
    const rows: [string, number][] = [
      ["context tokens", explain.context_tokens],
      ["budget tokens", explain.budget_tokens],
      ["messages", explain.messages],
      ["tools", explain.tools],
      ["compactions", explain.compactions],
    ];
    return [{ kind: "notice", level: "info",
      text: rows.map(([what, howMuch]) => `${what}: ${howMuch}`).join("\n") }];
  }

  /** What a reopened session shows: where you were, not every word ever said. */
  resumed(events: EventEnvelope[], sessionId: string): UiPart[] {
    if (events.length === 0) {
      return [{ kind: "notice", level: "info",
        text: `Resumed session \`${sessionId}\` — the log has no events yet.` }];
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
    const lines = [`Resumed session \`${sessionId}\` — ${events.length} events in the log.`];
    const asked = newest("user");
    const answered = newest("assistant");
    if (asked !== undefined) lines.push(`\n**You last said:** ${asked}`);
    if (answered !== undefined) lines.push(`\n**The agent last said:** ${answered}`);
    return [{ kind: "notice", level: "info", text: lines.join("\n") }];
  }

  /** Every path this turn mentioned; the caller filters unchanged files. */
  touchedFiles(): string[] {
    return [...this.files];
  }

  private toolCall(call: ToolCall): UiPart[] {
    const paths = pathsInArgs(call.args, this.cwd);
    for (const path of paths) {
      if (!this.files.includes(path)) this.files.push(path);
    }
    this.calls.set(call.call_id, { tool: call.tool, actionClass: call.action_class });
    return [{
      kind: "tool",
      callId: call.call_id,
      tool: call.tool,
      actionClass: call.action_class,
      paths,
      status: "running",
    }];
  }

  private toolResult(result: ToolResult): UiPart[] {
    const output = result.output ?? "";
    const call = this.calls.get(result.call_id);
    const dropped = output.length > MAX_TOOL_OUTPUT_CHARS
      ? output.length - MAX_TOOL_OUTPUT_CHARS
      : undefined;
    return [{
      kind: "tool",
      callId: result.call_id,
      tool: call?.tool ?? "tool",
      actionClass: call?.actionClass ?? "",
      paths: [],
      status: result.ok ? "ok" : "failed",
      output: output.slice(0, MAX_TOOL_OUTPUT_CHARS),
      ...(dropped === undefined ? {} : { truncated: dropped }),
    }];
  }

  private message(message: MessageAppended): UiPart[] {
    if (message.role !== "assistant") return [];
    return [{ kind: "assistant", text: message.text, html: renderMarkdown(message.text) }];
  }
}
