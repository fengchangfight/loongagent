import * as vscode from "vscode";
import { pickBinary, resolveHostBinary } from "./binaryHost.ts";
import { ChatViewProvider } from "./chatView.ts";

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("loongagent");
  context.subscriptions.push(output);
  output.appendLine("loongagent: activated");

  const provider = new ChatViewProvider({ context, output });
  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider(ChatViewProvider.viewType, provider, {
      webviewOptions: { retainContextWhenHidden: true },
    }),
    { dispose: () => provider.dispose() },
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("loongagent.openChat", async () => {
      await vscode.commands.executeCommand("workbench.view.extension.loongagent");
    }),
    vscode.commands.registerCommand("loongagent.locateBinary", async () => {
      const path = await pickBinary();
      if (path === undefined) return;
      await context.globalState.update("loongagent.binaryPath", path);
      output.appendLine(`binary: the person picked ${path}`);
      const binary = await resolveHostBinary(context, output);
      if (binary === undefined) {
        output.appendLine("binary: the picked path did not resolve; check the file and try again");
        return;
      }
      provider.setBinary(binary);
    }),
    vscode.commands.registerCommand("loongagent.attachSelection", () =>
      provider.attachSelection()),
    vscode.commands.registerCommand("loongagent.reviewChanges", (token?: string, paths?: string[]) =>
      provider.reviewChanges(token, paths)),
  );

  void resolveHostBinary(context, output).then(
    (binary) => {
      if (binary !== undefined) provider.setBinary(binary);
    },
    (error: unknown) => output.appendLine(`binary: resolution failed: ${String(error)}`),
  );
}

export function deactivate(): void {}
