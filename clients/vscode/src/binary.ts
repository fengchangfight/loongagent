/** Where the resolved executable came from, so a failure can name the choice. */
export type BinarySource = "setting" | "picked" | "path" | "candidate";

/** A resolved path and the rule that found it. */
export interface ResolvedBinary {
  path: string;
  source: BinarySource;
}

/**
 * The environment the resolver reads.
 *
 * Injected rather than imported so the decision is testable on any platform and
 * without touching the filesystem. `exists` and `isExecutable` are the only
 * filesystem facts; `pathEnv` is the caller's PATH.
 */
export interface BinaryEnv {
  configured: string;
  picked?: string;
  platform: NodeJS.Platform;
  home: string;
  pathEnv: string | undefined;
  exists(path: string): boolean;
  isExecutable(path: string): boolean;
}

/** The executable's name on this platform. */
export function binaryName(platform: NodeJS.Platform): string {
  return platform === "win32" ? "loongagent.exe" : "loongagent";
}

/** The usual install locations, in the order the resolver tries them. */
export function candidatePaths(platform: NodeJS.Platform, home: string): string[] {
  const name = binaryName(platform);
  const win = platform === "win32";
  const roots = win
    ? [`${home}\\.cargo\\bin`, `${home}\\.local\\bin`,
       `${home}\\AppData\\Local\\Programs\\loongagent`]
    : [`${home}/.cargo/bin`, `${home}/.local/bin`,
       "/usr/local/bin", "/opt/homebrew/bin"];
  return roots.map((root) => `${root}${win ? "\\" : "/"}${name}`);
}

/** Whether the value names a path rather than a bare command. */
function isPathLike(value: string): boolean {
  return value.startsWith("/") || value.startsWith("\\\\") || /^[A-Za-z]:[\\/]/.test(value);
}

/** Join a directory and a name using the target platform's separator. */
function joinPath(platform: NodeJS.Platform, directory: string, name: string): string {
  const separator = platform === "win32" ? "\\" : "/";
  return `${directory.replace(/[\\/]+$/, "")}${separator}${name}`;
}

/**
 * Find the binary: configured path, the user's Locate choice, PATH, then the
 * common install locations. A missing explicit path falls through rather than
 * aborting; that is what lets a stale setting be repaired by Locate.
 */
export function resolveBinary(env: BinaryEnv): ResolvedBinary | undefined {
  const executable = (path: string): boolean => env.exists(path) && env.isExecutable(path);
  const configured = env.configured.trim();
  if (configured !== "" && isPathLike(configured) && executable(configured)) {
    return { path: configured, source: "setting" };
  }

  const picked = env.picked?.trim() ?? "";
  if (picked !== "" && executable(picked)) {
    return { path: picked, source: "picked" };
  }

  const name = configured !== "" && !isPathLike(configured)
    ? configured
    : binaryName(env.platform);
  const separator = env.platform === "win32" ? ";" : ":";
  for (const directory of (env.pathEnv ?? "").split(separator)) {
    if (directory === "") continue;
    const candidate = joinPath(env.platform, directory, name);
    if (executable(candidate)) return { path: candidate, source: "path" };
  }

  for (const candidate of candidatePaths(env.platform, env.home)) {
    if (executable(candidate)) return { path: candidate, source: "candidate" };
  }
  return undefined;
}

/** Parse `loongagent 0.0.1 (schema 0.1.0, commit abc)`; anything else is unknown. */
export function parseVersion(stdout: string): {
  version: string;
  schema: string;
  commit?: string;
} | undefined {
  const match = /^loongagent\s+(\S+)\s+\(schema\s+([^\s,)]+)(?:,\s+commit\s+([^)]+))?\)/.exec(stdout);
  if (match === null) return undefined;
  return {
    version: match[1],
    schema: match[2],
    ...(match[3] === undefined ? {} : { commit: match[3] }),
  };
}
