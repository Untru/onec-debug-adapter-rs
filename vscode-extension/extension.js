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

function configuredAutoAttachTypes(session) {
  const configurations = vscode.workspace
    .getConfiguration("launch")
    .get("configurations", []);
  const configuration = configurations.find(
    (item) => item && item.type === "onec" && item.name === session.configuration.name
  );
  return Array.isArray(configuration?.autoAttachTypes)
    ? configuration.autoAttachTypes
    : [];
}

class DebugTargetsProvider {
  constructor() {
    this.items = [];
    this.changeEmitter = new vscode.EventEmitter();
    this.onDidChangeTreeData = this.changeEmitter.event;
  }

  update(items) {
    this.items = Array.isArray(items)
      ? items.filter((item) => item && typeof (item.Id ?? item.id) === "string")
      : [];
    this.changeEmitter.fire();
  }

  getTreeItem(target) {
    const id = target.Id ?? target.id;
    const type = target.Type ?? target.type ?? "Неизвестный тип";
    const user = target.User ?? target.user ?? "Неизвестный пользователь";
    const seance = target.Seance ?? target.seance ?? "";
    const item = new vscode.TreeItem(
      `${type} (${user}${seance ? `, ${seance}` : ""})`,
      vscode.TreeItemCollapsibleState.None
    );
    item.id = id;
    item.contextValue = "onecDebugTarget";
    item.command = {
      command: "debug.debugTargets.connect",
      title: "Подключить",
      arguments: [target]
    };
    return item;
  }

  getChildren(element) {
    return element ? [] : this.items;
  }
}

const debugTargetsProvider = new DebugTargetsProvider();

function activeOnecDebugSession() {
  const session = vscode.debug.activeDebugSession;
  return session?.type === "onec" ? session : undefined;
}

function updateDebugTargets(session) {
  if (session?.type !== "onec") {
    return Promise.resolve();
  }
  return session
    .customRequest("DebugTargetsRequest")
    .then((targets) => debugTargetsProvider.update(targets?.Items ?? targets?.items))
    .catch(() => undefined);
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
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("debug.debugTargets", debugTargetsProvider)
  );
  context.subscriptions.push(
    vscode.commands.registerCommand("debug.debugTargets.refresh", () =>
      updateDebugTargets(activeOnecDebugSession())
    )
  );
  context.subscriptions.push(
    vscode.commands.registerCommand("debug.debugTargets.connect", (target) => {
      const id = target?.Id ?? target?.id;
      const session = activeOnecDebugSession();
      if (typeof id !== "string" || !session) {
        return undefined;
      }
      return session
        .customRequest("AttachDebugTargetRequest", { Id: id })
        .then(() => updateDebugTargets(session))
        .catch(() => undefined);
    })
  );
  context.subscriptions.push(
    vscode.debug.onDidStartDebugSession((session) => {
      updateDebugTargets(session);
    })
  );
  context.subscriptions.push(
    vscode.debug.onDidTerminateDebugSession((session) => {
      if (session.type === "onec") {
        debugTargetsProvider.update([]);
      }
    })
  );
  context.subscriptions.push(
    vscode.debug.onDidReceiveDebugSessionCustomEvent((debugEvent) => {
      if (
        debugEvent.session.type === "onec" &&
        debugEvent.event === "DebugTargetsUpdated"
      ) {
        updateDebugTargets(debugEvent.session);
      }
    })
  );
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((event) => {
      const session = vscode.debug.activeDebugSession;
      if (
        session?.type === "onec" &&
        event.affectsConfiguration("launch.configurations")
      ) {
        session
          .customRequest("SetAutoAttachTargetTypesRequest", {
            types: configuredAutoAttachTypes(session)
          })
          .then(undefined, () => undefined);
      }
    })
  );
}

function deactivate() {}

module.exports = {
  activate,
  deactivate,
  adapterPath,
  bundledAdapterPath,
  configuredAutoAttachTypes,
  DebugTargetsProvider,
  debugTargetsProvider,
  updateDebugTargets,
  platformBinaryName
};
