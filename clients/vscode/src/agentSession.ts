import { spawn } from "node:child_process";
import { StringDecoder } from "node:string_decoder";

/**
 * What this transport needs from a child process, and nothing more.
 *
 * Structural rather than a `ChildProcess`, for one reason: a fake can satisfy it
 * without pretending to be a process, so the framing and correlation rules are
 * tested by feeding bytes rather than by spawning something.
 */
export interface ChildLike {
  stdin: { write(chunk: string): unknown } | null;
  stdout: { on(event: "data", listener: (chunk: Buffer | string) => void): unknown } | null;
  stderr: { on(event: "data", listener: (chunk: Buffer | string) => void): unknown } | null;
  on(event: "exit", listener: (code: number | null, signal: string | null) => void): unknown;
  on(event: "error", listener: (error: Error) => void): unknown;
  kill(signal?: NodeJS.Signals): unknown;
}

export interface AgentSessionHooks {
  /** Every JSON-RPC notification the server sends (method + params, verbatim). */
  onNotification(method: string, params: unknown): void;
  /** Anything the transport cannot turn into a message: child stderr, malformed lines. */
  onLog(line: string): void;
  onExit(code: number | null, signal: string | null): void;
}

/** A JSON-RPC error response. The code is the server's, never one we invented. */
export class JsonRpcError extends Error {
  readonly code: number;
  readonly data?: unknown;

  // No parameter properties: `node --test` strips types rather than compiling them,
  // so a constructor that declares its fields is a syntax error at test time.
  constructor(code: number, message: string, data?: unknown) {
    super(message);
    this.name = "JsonRpcError";
    this.code = code;
    this.data = data;
  }
}

interface Pending {
  resolve(value: unknown): void;
  reject(error: Error): void;
}

interface Incoming {
  id?: unknown;
  method?: unknown;
  params?: unknown;
  result?: unknown;
  error?: { code?: unknown; message?: unknown; data?: unknown };
}

/**
 * One JSON-RPC 2.0 connection over a newline-delimited stream.
 *
 * Requests are correlated by id, not by order, and a request may be sent while
 * another is still outstanding. That is not a nicety: a turn stops at an
 * approval boundary and returns, so answering it means writing `approval.respond`
 * on this same connection while the prompt it belongs to is still open.
 */
export class AgentSession {
  private readonly child: ChildLike;
  private readonly hooks: AgentSessionHooks;
  private readonly pending = new Map<number, Pending>();
  private nextId = 1;
  private exited = false;
  private disposed = false;
  private readonly outDecoder = new StringDecoder("utf8");
  private readonly errDecoder = new StringDecoder("utf8");
  private outBuffer = "";
  private errBuffer = "";

  constructor(child: ChildLike, hooks: AgentSessionHooks) {
    this.child = child;
    this.hooks = hooks;
    child.stdout?.on("data", (chunk) => this.readStdout(chunk));
    child.stderr?.on("data", (chunk) => this.readStderr(chunk));
    child.on("exit", (code, signal) => this.gone(code, signal));
    child.on("error", (error) => this.broke(error));
  }

  /** Send one request and resolve with its result. */
  request<T = unknown>(method: string, params: Record<string, unknown>): Promise<T> {
    if (this.exited) {
      return Promise.reject(new Error("the loongagent server is gone; nothing was sent"));
    }
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
      this.write({ jsonrpc: "2.0", id, method, params });
    });
  }

  /** Stop the running turn. The server survives: Ctrl-C stops a turn, not the process. */
  cancel(): void {
    this.child.kill("SIGINT");
  }

  /** Stop reading and end the child. Pending requests are rejected, not left hanging. */
  async dispose(): Promise<void> {
    this.disposed = true;
    this.rejectAll(new Error("the session was disposed"));
    if (!this.exited) {
      this.exited = true;
      this.child.kill();
    }
  }

  private write(message: Record<string, unknown>): void {
    this.child.stdin?.write(JSON.stringify(message) + "\n");
  }

  private readStdout(chunk: Buffer | string): void {
    this.outBuffer += typeof chunk === "string" ? chunk : this.outDecoder.write(chunk);
    let newline = this.outBuffer.indexOf("\n");
    while (newline >= 0) {
      const line = this.outBuffer.slice(0, newline);
      this.outBuffer = this.outBuffer.slice(newline + 1);
      if (line.trim() !== "") this.receive(line);
      newline = this.outBuffer.indexOf("\n");
    }
  }

  private readStderr(chunk: Buffer | string): void {
    this.errBuffer += typeof chunk === "string" ? chunk : this.errDecoder.write(chunk);
    let newline = this.errBuffer.indexOf("\n");
    while (newline >= 0) {
      const line = this.errBuffer.slice(0, newline);
      this.errBuffer = this.errBuffer.slice(newline + 1);
      if (line.trim() !== "") this.hooks.onLog(line);
      newline = this.errBuffer.indexOf("\n");
    }
  }

  private receive(line: string): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      this.hooks.onLog(`unparsable line: ${line}`);
      return;
    }
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
      this.hooks.onLog(`unparsable line: ${line}`);
      return;
    }
    const message = parsed as Incoming;
    if (typeof message.method === "string") {
      this.hooks.onNotification(message.method, message.params);
      if (message.id !== undefined) {
        // Nothing in this protocol asks the client anything, so a server request
        // is not answered — but it is not silently dropped either.
        this.hooks.onLog(`ignoring a server request for ${message.method}: this client answers nothing`);
      }
      return;
    }
    if (typeof message.id !== "number") {
      this.hooks.onLog(`unparsable line: ${line}`);
      return;
    }
    const entry = this.pending.get(message.id);
    if (entry === undefined) {
      this.hooks.onLog(`no request is waiting for id ${message.id}`);
      return;
    }
    this.pending.delete(message.id);
    const failure = message.error;
    if (failure !== undefined) {
      entry.reject(
        new JsonRpcError(
          typeof failure.code === "number" ? failure.code : 0,
          typeof failure.message === "string" ? failure.message : "the server refused without saying why",
          failure.data,
        ),
      );
      return;
    }
    entry.resolve(message.result);
  }

  /** The child is gone: the reason travels with every request that was waiting. */
  private gone(code: number | null, signal: string | null): void {
    if (this.exited || this.disposed) return;
    this.exited = true;
    const reason = signal !== null
      ? `the loongagent server was killed by ${signal}`
      : `the loongagent server exited with code ${code}`;
    this.rejectAll(new Error(reason));
    this.hooks.onExit(code, signal);
  }

  /** The child never started (ENOENT) or the pipe broke. */
  private broke(error: Error): void {
    if (this.exited || this.disposed) return;
    this.exited = true;
    this.hooks.onLog(error.message);
    this.rejectAll(error);
    this.hooks.onExit(null, null);
  }

  private rejectAll(error: Error): void {
    for (const entry of this.pending.values()) entry.reject(error);
    this.pending.clear();
  }
}

/**
 * Start the server and speak to it. The working directory is always the caller's:
 * the extension host's own `process.cwd()` is not documented to be the workspace
 * root, so nothing here reads it.
 */
export function spawnAgent(
  command: string,
  args: string[],
  cwd: string,
  hooks: AgentSessionHooks,
): AgentSession {
  const child = spawn(command, args, { cwd, stdio: ["pipe", "pipe", "pipe"] });
  return new AgentSession(child as ChildLike, hooks);
}
