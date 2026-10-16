import { EventEmitter } from "node:events";
import type { ChildLike } from "../src/agentSession.ts";

export class FakeChild implements ChildLike {
  readonly written: string[] = [];
  readonly signals: (NodeJS.Signals | undefined)[] = [];
  readonly stdout = new EventEmitter();
  readonly stderr = new EventEmitter();
  private readonly events = new EventEmitter();
  readonly stdin = { write: (chunk: string) => { this.written.push(chunk); return true; } };
  on(event: "exit" | "error", listener: (...args: never[]) => void): this {
    this.events.on(event, listener as (...args: unknown[]) => void); return this;
  }
  kill(signal?: NodeJS.Signals): boolean { this.signals.push(signal); return true; }
  /** Feed raw bytes as the child's stdout. */
  emitStdout(bytes: string): void { this.stdout.emit("data", Buffer.from(bytes, "utf8")); }
  emitExit(code: number | null, signal: NodeJS.Signals | null = null): void {
    this.events.emit("exit", code, signal);
  }
  /** The JSON objects the session has written so far. */
  sent(): { id?: number; method: string; params: unknown }[] {
    return this.written.join("").split("\n").filter(Boolean).map((l) => JSON.parse(l));
  }
}
