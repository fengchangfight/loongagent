/**
 * Why the agent will not start here, or nothing when it may.
 *
 * One function for every entry point, so a new way in (a command, a second
 * surface) cannot be less careful than the participant — the rule lives here
 * rather than in each caller (spec §4.7).
 */
export function refusalToStart(options: {
  hasFolder: boolean;
  isTrusted: boolean;
}): string | undefined {
  if (!options.hasFolder) {
    return "Open a folder first — the agent works on a workspace.";
  }
  if (!options.isTrusted) {
    return "This workspace is not trusted, so I will not start the agent here. "
      + "Trust the folder (Workspaces: Manage Workspace Trust) and ask again.";
  }
  return undefined;
}
