import * as vscode from "vscode";

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("loongagent");
  context.subscriptions.push(output);
  output.appendLine("loongagent: activated");
}

export function deactivate(): void {}
