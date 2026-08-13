const fs = require("node:fs");
const path = require("node:path");
const vscode = require("vscode");
const {
  configurationSummary,
  hasCredentials,
  isSamePath,
  parseExtensionName,
  uniqueConfigurationName
} = require("./setup-wizard");

const IGNORED_DISCOVERY_DIRECTORIES = new Set([
  ".git",
  "node_modules",
  "target",
  ".build",
  "artifacts"
]);

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

async function canonicalDirectory(directory) {
  const canonical = await fs.promises.realpath(directory);
  const stat = await fs.promises.stat(canonical);
  if (!stat.isDirectory()) {
    throw new Error("Укажите каталог, а не файл.");
  }
  return canonical;
}

async function configurationRoot(directory) {
  const canonical = await canonicalDirectory(directory);
  const descriptor = path.join(canonical, "Configuration.xml");
  const stat = await fs.promises.stat(descriptor).catch(() => undefined);
  if (!stat?.isFile()) {
    throw new Error("В каталоге не найден файл Configuration.xml.");
  }
  return canonical;
}

async function extensionRoot(directory) {
  const root = await configurationRoot(directory);
  const xml = await fs.promises.readFile(path.join(root, "Configuration.xml"), "utf8");
  const name = parseExtensionName(xml);
  if (!name) {
    throw new Error("Не удалось прочитать <Properties><Name> из Configuration.xml.");
  }
  return { name, path: root };
}

async function discoverConfigurationRoots(root) {
  const candidates = [];
  const directories = [root];
  while (directories.length) {
    const directory = directories.pop();
    const entries = await fs.promises.readdir(directory, { withFileTypes: true }).catch(() => []);
    if (entries.some((entry) => entry.isFile() && entry.name === "Configuration.xml")) {
      candidates.push(directory);
    }
    for (const entry of entries) {
      if (entry.isDirectory() && !IGNORED_DISCOVERY_DIRECTORIES.has(entry.name)) {
        directories.push(path.join(directory, entry.name));
      }
    }
  }
  return candidates.sort((first, second) => first.localeCompare(second));
}

function formatPathForWorkspace(directory, workspaceFolder) {
  const relative = path.relative(workspaceFolder.uri.fsPath, directory);
  return relative && !relative.startsWith("..") && !path.isAbsolute(relative)
    ? relative
    : directory;
}

async function chooseWorkspaceFolder() {
  const folders = vscode.workspace.workspaceFolders ?? [];
  if (folders.length === 1) {
    return folders[0];
  }
  if (folders.length === 0) {
    const action = await vscode.window.showWarningMessage(
      "Для настройки отладки сначала откройте папку с исходниками 1С.",
      "Открыть папку…"
    );
    if (action) {
      await vscode.commands.executeCommand("vscode.openFolder");
    }
    return undefined;
  }
  const selected = await vscode.window.showQuickPick(
    folders.map((folder) => ({
      label: folder.name,
      description: folder.uri.fsPath,
      folder
    })),
    {
      title: "1C: Выберите папку рабочей области",
      placeHolder: "В эту папку будет сохранён launch.json"
    }
  );
  return selected?.folder;
}

async function pickDirectory(title, defaultUri, canSelectMany = false) {
  const selected = await vscode.window.showOpenDialog({
    title,
    defaultUri,
    canSelectFiles: false,
    canSelectFolders: true,
    canSelectMany,
    openLabel: canSelectMany ? "Выбрать каталоги" : "Выбрать каталог"
  });
  return selected?.map((item) => item.fsPath);
}

async function chooseBaseProject(workspaceFolder) {
  const discovered = await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Window,
      title: "1C: Поиск исходников конфигурации"
    },
    () => discoverConfigurationRoots(workspaceFolder.uri.fsPath)
  );
  const picked = await vscode.window.showQuickPick(
    [
      ...discovered.map((directory) => ({
        label: `$(folder) ${formatPathForWorkspace(directory, workspaceFolder)}`,
        description: directory,
        directory
      })),
      {
        label: "$(folder-opened) Выбрать другой каталог…",
        description: "Каталог должен содержать Configuration.xml",
        browse: true
      }
    ],
    {
      title: "1C: Исходники основной конфигурации",
      placeHolder: "Выберите каталог с Configuration.xml"
    }
  );
  if (!picked) {
    return undefined;
  }
  const directory = picked.browse
    ? (await pickDirectory("1C: Исходники основной конфигурации", workspaceFolder.uri))?.[0]
    : picked.directory;
  if (!directory) {
    return undefined;
  }
  try {
    return await configurationRoot(directory);
  } catch (error) {
    await vscode.window.showErrorMessage(`Не удалось выбрать основную конфигурацию: ${error.message}`);
    return chooseBaseProject(workspaceFolder);
  }
}

async function discoveredExtensionCandidates(workspaceFolder, baseProject) {
  const roots = await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Window,
      title: "1C: Поиск исходников расширений"
    },
    () => discoverConfigurationRoots(workspaceFolder.uri.fsPath)
  );
  const candidates = await Promise.all(
    roots
      .filter((root) => !isSamePath(root, baseProject))
      .map((root) => extensionRoot(root).catch(() => undefined))
  );
  return candidates.filter(Boolean);
}

function duplicateExtensionName(extensions) {
  const names = new Map();
  for (const extension of extensions) {
    const key = extension.name.toLocaleLowerCase();
    if (names.has(key)) {
      return extension.name;
    }
    names.set(key, extension.path);
  }
  return undefined;
}

async function chooseExtensions(workspaceFolder, baseProject) {
  const discovered = await discoveredExtensionCandidates(workspaceFolder, baseProject);
  let selectedPaths = [];
  while (true) {
    const knownByPath = new Map(discovered.map((item) => [item.path, item]));
    const selected = await vscode.window.showQuickPick(
      [
        ...discovered.map((extension) => ({
          label: extension.name,
          description: formatPathForWorkspace(extension.path, workspaceFolder),
          detail: extension.path,
          path: extension.path,
          picked: selectedPaths.some((item) => isSamePath(item, extension.path))
        })),
        ...selectedPaths
          .filter((item) => !knownByPath.has(item))
          .map((item) => ({
            label: `$(folder) ${path.basename(item)}`,
            description: "Выбранный внешний каталог",
            detail: item,
            path: item,
            picked: true
          })),
        {
          label: "$(folder-opened) Добавить каталоги вне рабочей области…",
          description: "Каждый каталог должен содержать Configuration.xml",
          browse: true
        }
      ],
      {
        title: "1C: Исходники расширений",
        placeHolder: "Необязательно: выберите расширения, которые нужно отлаживать",
        canPickMany: true
      }
    );
    if (!selected) {
      return undefined;
    }
    const selectedItems = selected.filter((item) => !item.browse);
    selectedPaths = selectedItems.map((item) => item.path);
    if (selected.some((item) => item.browse)) {
      const additional = await pickDirectory(
        "1C: Дополнительные исходники расширений",
        workspaceFolder.uri,
        true
      );
      if (additional) {
        selectedPaths.push(...additional);
      }
    }
    try {
      const extensions = [];
      for (const directory of selectedPaths) {
        const extension = await extensionRoot(directory);
        if (!isSamePath(extension.path, baseProject) && !extensions.some(
          (item) => isSamePath(item.path, extension.path)
        )) {
          extensions.push(extension);
        }
      }
      const duplicateName = duplicateExtensionName(extensions);
      if (duplicateName) {
        await vscode.window.showErrorMessage(
          `Расширение «${duplicateName}» выбрано более одного раза. Оставьте только один каталог с этим именем.`
        );
        selectedPaths = extensions.map((item) => item.path);
        continue;
      }
      return extensions;
    } catch (error) {
      await vscode.window.showErrorMessage(`Не удалось добавить расширение: ${error.message}`);
    }
  }
}

async function chooseRequest() {
  const selected = await vscode.window.showQuickPick(
    [
      {
        label: "Запуск 1С",
        description: "Запустить клиент 1С и подключить отладчик",
        request: "launch"
      },
      {
        label: "Подключение к отладчику",
        description: "Подключиться к уже доступному серверу отладки",
        request: "attach"
      }
    ],
    { title: "1C: Режим отладки" }
  );
  return selected?.request;
}

async function inputInfoBase(title, prompt) {
  while (true) {
    const value = await vscode.window.showInputBox({
      title,
      prompt,
      ignoreFocusOut: true,
      validateInput: (input) => {
        if (!input.trim()) return "Значение обязательно.";
        if (hasCredentials(input)) return "Не указывайте учётные данные в launch.json.";
        return undefined;
      }
    });
    if (value === undefined) return undefined;
    if (!hasCredentials(value) && value.trim()) return value.trim();
  }
}

async function chooseLaunchInfoBase(workspaceFolder) {
  const selected = await vscode.window.showQuickPick(
    [
      {
        label: "Файловая информационная база",
        description: "Выбрать существующий каталог файловой базы",
        kind: "file"
      },
      {
        label: "Зарегистрированная информационная база",
        description: "Ввести имя базы из списка запуска 1С",
        kind: "registered"
      }
    ],
    { title: "1C: Информационная база для запуска" }
  );
  if (!selected) return undefined;
  if (selected.kind === "registered") {
    return inputInfoBase(
      "1C: Имя зарегистрированной базы",
      "Введите только имя базы, без строки подключения и пароля"
    );
  }
  const directory = (await pickDirectory("1C: Каталог файловой информационной базы", workspaceFolder.uri))?.[0];
  if (!directory) return undefined;
  try {
    return await canonicalDirectory(directory);
  } catch (error) {
    await vscode.window.showErrorMessage(`Не удалось выбрать файловую базу: ${error.message}`);
    return chooseLaunchInfoBase(workspaceFolder);
  }
}

function platformExecutables() {
  const suffix = process.platform === "win32" ? ".exe" : "";
  return { client: `1cv8c${suffix}`, debugServer: `dbgs${suffix}` };
}

async function containsPlatformBinaries(directory) {
  const { client, debugServer } = platformExecutables();
  const [clientStat, serverStat] = await Promise.all(
    [client, debugServer].map((name) => fs.promises.stat(path.join(directory, name)).catch(() => undefined))
  );
  return Boolean(clientStat?.isFile() && serverStat?.isFile());
}

function isPlatformVersionDirectory(name) {
  return name.split(".").every((part) => /^\d+$/.test(part));
}

function platformBinaryDirectory(versionDirectory) {
  return process.platform === "win32" ? path.join(versionDirectory, "bin") : versionDirectory;
}

async function validatePlatformDirectory(directory) {
  const root = await canonicalDirectory(directory);
  if (await containsPlatformBinaries(root)) return root;
  const children = await fs.promises.readdir(root, { withFileTypes: true });
  const candidates = await Promise.all(
    children
      .filter((entry) => entry.isDirectory() && isPlatformVersionDirectory(entry.name))
      .map(async (entry) => {
        const child = platformBinaryDirectory(path.join(root, entry.name));
        return (await containsPlatformBinaries(child)) ? child : undefined;
      })
  );
  if (candidates.some(Boolean)) return root;
  const { client, debugServer } = platformExecutables();
  throw new Error(`Не найдены ${client} и ${debugServer} ни в каталоге, ни в его каталогах версий.`);
}

async function choosePlatformDirectory(workspaceFolder) {
  while (true) {
    const selected = await pickDirectory("1C: Каталог платформы 1С", workspaceFolder.uri);
    if (!selected) return undefined;
    try {
      return await validatePlatformDirectory(selected[0]);
    } catch (error) {
      await vscode.window.showErrorMessage(`Неверный каталог платформы: ${error.message}`);
    }
  }
}

async function chooseDebugServer() {
  const host = await vscode.window.showInputBox({
    title: "1C: Сервер отладки",
    prompt: "Адрес сервера отладки",
    value: "localhost",
    ignoreFocusOut: true,
    validateInput: (input) => (input.trim() ? undefined : "Укажите адрес сервера.")
  });
  if (host === undefined) return undefined;
  const portValue = await vscode.window.showInputBox({
    title: "1C: Порт сервера отладки",
    value: "1550",
    ignoreFocusOut: true,
    validateInput: (input) => {
      const port = Number(input);
      return Number.isInteger(port) && port >= 1 && port <= 65535
        ? undefined
        : "Введите номер порта от 1 до 65535.";
    }
  });
  if (portValue === undefined) return undefined;
  return { host: host.trim(), port: Number(portValue) };
}

async function chooseOptionalAlias() {
  const alias = await vscode.window.showInputBox({
    title: "1C: Псевдоним информационной базы",
    prompt: "Необязательно. Оставьте пустым, если псевдоним не нужен.",
    ignoreFocusOut: true,
    validateInput: (input) => hasCredentials(input)
      ? "Не указывайте учётные данные в launch.json."
      : undefined
  });
  return alias === undefined ? undefined : { value: alias.trim() || undefined };
}

async function launchConfigurationsFor(folder) {
  const configurations = vscode.workspace
    .getConfiguration("launch", folder.uri)
    .get("configurations", []);
  return Array.isArray(configurations) ? configurations : [];
}

async function configureDebugger() {
  const workspaceFolder = await chooseWorkspaceFolder();
  if (!workspaceFolder) return;
  const rootProject = await chooseBaseProject(workspaceFolder);
  if (!rootProject) return;
  const extensions = await chooseExtensions(workspaceFolder, rootProject);
  if (!extensions) return;
  const request = await chooseRequest();
  if (!request) return;

  const infoBase = request === "launch"
    ? await chooseLaunchInfoBase(workspaceFolder)
    : await inputInfoBase(
      "1C: Информационная база для подключения",
      "Введите имя базы или её серверный идентификатор, без учётных данных"
    );
  if (!infoBase) return;
  const debugServer = await chooseDebugServer();
  if (!debugServer) return;
  const aliasResult = request === "attach" ? await chooseOptionalAlias() : { value: undefined };
  if (!aliasResult) return;
  const platformPath = request === "launch" ? await choosePlatformDirectory(workspaceFolder) : undefined;
  if (request === "launch" && !platformPath) return;

  const existing = await launchConfigurationsFor(workspaceFolder);
  const mode = request === "launch" ? "запуск" : "подключение";
  const configuration = {
    name: uniqueConfigurationName(existing, `1C: ${path.basename(rootProject)} (${mode})`),
    type: "onec",
    request,
    rootProject,
    infoBase,
    debugServerHost: debugServer.host,
    debugServerPort: debugServer.port,
    autoAttachTypes: ["ManagedClient", "Server"]
  };
  if (aliasResult.value) configuration.infoBaseAlias = aliasResult.value;
  if (extensions.length) configuration.extensions = extensions.map((extension) => extension.path);
  if (request === "launch") {
    configuration.platformPath = platformPath;
    configuration.platformVersion = "LATEST";
  }

  const confirmation = await vscode.window.showInformationMessage(
    "Создать эту конфигурацию отладки 1С?",
    { modal: true, detail: configurationSummary(configuration) },
    "Создать конфигурацию"
  );
  if (confirmation !== "Создать конфигурацию") return;

  const latest = await launchConfigurationsFor(workspaceFolder);
  configuration.name = uniqueConfigurationName(
    latest.filter((item) => item !== configuration),
    configuration.name
  );
  try {
    await vscode.workspace
      .getConfiguration("launch", workspaceFolder.uri)
      .update(
        "configurations",
        [...latest, configuration],
        vscode.ConfigurationTarget.WorkspaceFolder
      );
    await vscode.window.showInformationMessage(
      `Конфигурация «${configuration.name}» добавлена в ${workspaceFolder.name}/.vscode/launch.json.`
    );
  } catch (error) {
    await vscode.window.showErrorMessage(`Не удалось записать launch.json: ${error.message}`);
  }
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
    vscode.commands.registerCommand("onec.configureDebugger", configureDebugger)
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
  platformBinaryName,
  canonicalDirectory,
  configurationRoot,
  discoverConfigurationRoots,
  extensionRoot,
  isPlatformVersionDirectory,
  validatePlatformDirectory
};
