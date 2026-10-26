import * as vscode from "vscode";
import { execFile } from "node:child_process";
import { accessSync, constants, existsSync } from "node:fs";
import { homedir } from "node:os";
import { promisify } from "node:util";
import { parseVersion, resolveBinary } from "./binary.ts";
import { SCHEMA_VERSION } from "../../ts/protocol.generated.ts";
import type { BinaryStatus } from "./webviewProtocol.ts";

const run = promisify(execFile);
const PICKED_KEY = "loongagent.binaryPath";

/** The resolved binary and the version information shown in the sidebar. */
export type HostBinary = BinaryStatus;

/**
 * Find the binary and prove it answers `--version`.
 *
 * A missing binary returns `undefined` and leaves an actionable line in the
 * output channel; it never throws into activation. The schema mismatch is a
 * warning, not a refusal: the wire may still work.
 */
export async function resolveHostBinary(
  context: vscode.ExtensionContext,
  output: vscode.OutputChannel,
): Promise<HostBinary | undefined> {
  const configured = vscode.workspace.getConfiguration("loongagent").get<string>("path", "loongagent");
  const picked = context.globalState.get<string>(PICKED_KEY);
  const resolved = resolveBinary({
    configured,
    picked,
    platform: process.platform,
    home: homedir(),
    pathEnv: process.env.PATH,
    exists: (path) => existsSync(path),
    isExecutable: (path) => process.platform === "win32"
      ? existsSync(path)
      : (() => { try { accessSync(path, constants.X_OK); return true; } catch { return false; } })(),
  });
  if (resolved === undefined) {
    output.appendLine(
      `binary: not found (setting ${JSON.stringify(configured)}; ` +
      "set loongagent.path or run \"loongagent: Locate the binary\")",
    );
    return undefined;
  }

  let version = "unknown";
  let schema = "unknown";
  try {
    const { stdout } = await run(resolved.path, ["--version"], {
      timeout: 5000,
      windowsHide: true,
    });
    const parsed = parseVersion(String(stdout));
    if (parsed === undefined) {
      output.appendLine(`binary: ${resolved.path} answered --version with an unknown format`);
    } else {
      version = parsed.version;
      schema = parsed.schema;
    }
  } catch (error) {
    output.appendLine(`binary: ${resolved.path} failed --version: ${explain(error)}`);
  }
  if (schema !== "unknown" && schema !== SCHEMA_VERSION) {
    output.appendLine(
      `warning: binary schema ${schema} differs from the extension schema ${SCHEMA_VERSION}`,
    );
  }
  output.appendLine(`binary: ${resolved.path} (${resolved.source}); version ${version}, schema ${schema}`);
  return { path: resolved.path, version, schema };
}

/** Ask the person for a binary; the caller stores the path. */
export async function pickBinary(): Promise<string | undefined> {
  const picked = await vscode.window.showOpenDialog({
    canSelectFiles: true,
    canSelectFolders: false,
    canSelectMany: false,
    openLabel: "Use this binary",
  });
  return picked?.[0]?.fsPath;
}

function explain(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}
