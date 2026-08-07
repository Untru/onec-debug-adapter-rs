const fs = require("node:fs");
const path = require("node:path");
const vscode = require("vscode");

function platformBinaryName() {
  return process.platform === "win32" ? "onec-debug-adapter.exe" : "onec-debug-adapter";
}

function bundledAdapterPath(extensionPath) {
  return path.join(
    extensionPath,
    "bin",
    `${process.platform}-${process.arch}`,
    platformBinaryName()
  );
}

function adapterPath(extensionPath) {
  const configuredPath = vscode.workspace
    .getConfiguration("onec")
    .get("nativeAdapterPath", "")
    .trim();
  return configuredPath || bundledAdapterPath(extensionPath);
}

function activate(context) {
  const factory = {
    createDebugAdapterDescriptor() {
      const executable = adapterPath(context.extensionPath);
      if (!fs.existsSync(executable)) {
        throw new Error(
          `The native 1C debug adapter was not found at ${executable}. ` +
            "Set onec.nativeAdapterPath while using a development build."
        );
      }
      return new vscode.DebugAdapterExecutable(executable, []);
    }
  };

  context.subscriptions.push(
    vscode.debug.registerDebugAdapterDescriptorFactory("onec", factory)
  );
}

function deactivate() {}

module.exports = { activate, deactivate, adapterPath, bundledAdapterPath, platformBinaryName };
