import { build, context } from "esbuild";

const extension = {
  entryPoints: ["src/extension.ts"],
  bundle: true,
  platform: "node",
  format: "cjs",
  target: "node20",
  external: ["vscode"],
  outfile: "dist/extension.js",
  sourcemap: true,
  logLevel: "info",
};

const webview = {
  entryPoints: ["webview/main.ts"],
  bundle: true,
  platform: "browser",
  format: "iife",
  target: "es2022",
  outfile: "dist/webview.js",
  sourcemap: true,
  logLevel: "info",
};

if (process.argv.includes("--watch")) {
  const [extensionContext, webviewContext] = await Promise.all([
    context(extension),
    context(webview),
  ]);
  await Promise.all([extensionContext.watch(), webviewContext.watch()]);
} else {
  await Promise.all([build(extension), build(webview)]);
}
