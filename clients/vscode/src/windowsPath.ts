//! Purpose: build a PATH string on Windows the way a fresh terminal does, so
//! the extension host — which inherits a stale PATH snapshot from the VS Code
//! process that launched it — sees the same binary as the TUI.
//! Serves: clients/vscode/src/binaryHost.ts
//! Design: `docs/design/vscode-surface.md` §5 Finding the binary
//! Upstream: docs/design/vscode-surface.md
//! Downstream: read by binaryHost.ts; the pure helpers are exercised by test/windowsPath.test.ts
//! Verify: cd clients/vscode && npm run compile && npm test

/**
 * Where `process.env.PATH` comes from and why it can be stale.
 *
 * The Windows loader copies the parent process's environment block at process
 * creation; a `PATH` edited after VS Code started never reaches the running
 * extension host. A terminal opened later reads the registry and sees the new
 * value, so the TUI finds `loongagent` while the extension does not. Reading
 * the machine and user `Path` levels gives the extension the same
 * "just opened a terminal" PATH.
 */

/** One read of the two PATH levels; a missing level is `undefined`. */
export interface PathLevels {
  machine: string | undefined;
  user: string | undefined;
}

/**
 * Expand `%VAR%` references without the shell.
 *
 * `cmd /c echo` would spawn a shell (the very thing this avoids), so the
 * expansion is done by hand: an unset variable is left verbatim, and a
 * variable that refers to itself is left verbatim to keep the pass finite.
 * Two passes cover the common case of one variable referencing another; a
 * pathological cycle is left as text rather than looped forever.
 */
export function expandVariables(value: string, env: Record<string, string | undefined>): string {
  const once = (input: string): string =>
    input.replace(/%([^%]+)%/g, (whole, name: string) => env[name] ?? whole);
  return once(once(value));
}

/**
 * Merge process, machine and user PATH into the order a fresh login uses.
 *
 * Windows prepends the machine PATH to the user PATH at logon; the process
 * PATH is the already-merged block, but on a stale snapshot it predates an
 * edit. Appending the registry halves and de-duplicating by exact string
 * leaves the existing entries first (so an in-session override still wins)
 * and adds only what the snapshot missed.
 */
export function mergePathEntries(
  process: string | undefined,
  machine: string | undefined,
  user: string | undefined,
): string {
  const seen = new Set<string>();
  const out: string[] = [];
  const push = (chunk: string | undefined): void => {
    if (chunk === undefined) return;
    for (const entry of chunk.split(";")) {
      const trimmed = entry.trim();
      if (trimmed === "" || seen.has(trimmed)) continue;
      seen.add(trimmed);
      out.push(trimmed);
    }
  };
  push(process);
  push(machine);
  push(user);
  return out.join(";");
}

/**
 * The PATH the resolver should search, rebuilt when it can be and the
 * process PATH when it cannot.
 *
 * Every failure — non-Windows, PowerShell missing, the value absent — falls
 * back to `process.env.PATH` unchanged, so a partial read never makes the
 * search worse than the status quo.
 */
export function effectivePathEnv(
  platform: NodeJS.Platform,
  processPath: string | undefined,
  readLevels: () => PathLevels,
  env: Record<string, string | undefined>,
): string {
  if (platform !== "win32") return processPath ?? "";
  let machine: string | undefined;
  let user: string | undefined;
  try {
    ({ machine, user } = readLevels());
  } catch {
    return processPath ?? "";
  }
  return mergePathEntries(
    processPath,
    machine === undefined ? undefined : expandVariables(machine, env),
    user === undefined ? undefined : expandVariables(user, env),
  );
}
