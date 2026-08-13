const fs = require("node:fs");
const net = require("node:net");
const path = require("node:path");
const vscode = require("vscode");
const {
  canonicalDirectory,
  configurationSummary,
  defaultPlatformRoots,
  fileInfoBaseArgument,
  discoverIbaseEntries,
  discoverV8ProjectEntries,
  discoverPlatformDirectories,
  hasCredentials,
  ibasesFilePickerChoice,
  isSamePath,
  mergeInfoBaseEntries,
  noExtensionSourceChoices,
  parseExtensionName,
  parseServerInfoBaseArgument,
  serverInfoBaseArgument,
  uniqueConfigurationName,
  isPlatformVersionDirectory,
  validatePlatformDirectory
} = require("./setup-wizard");
const { discoverInfoBaseExtensions } = require("./extension-inventory");

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

async function configurationRoot(directory) {
  const canonical = await canonicalDirectory(directory);
  const xmlDescriptor = path.join(canonical, "Configuration.xml");
  const edtManifest = path.join(canonical, "DT-INF", "PROJECT.PMF");
  const edtDescriptor = path.join(canonical, "src", "Configuration", "Configuration.mdo");
  const [xml, manifest, edt] = await Promise.all(
    [xmlDescriptor, edtManifest, edtDescriptor].map((item) => fs.promises.stat(item).catch(() => undefined))
  );
  if (!xml?.isFile() && !(manifest?.isFile() && edt?.isFile())) {
    throw new Error("В каталоге не найден Configuration.xml или EDT-проект (DT-INF/PROJECT.PMF и src/Configuration/Configuration.mdo).");
  }
  return canonical;
}

async function extensionRoot(directory) {
  const root = await configurationRoot(directory);
  const edt = await fs.promises.stat(path.join(root, "DT-INF", "PROJECT.PMF")).catch(() => undefined);
  const descriptor = edt?.isFile()
    ? path.join(root, "src", "Configuration", "Configuration.mdo")
    : path.join(root, "Configuration.xml");
  const xml = await fs.promises.readFile(descriptor, "utf8");
  const name = edt?.isFile()
    ? xml.match(/<name\b[^>]*>\s*([^<]+?)\s*<\/name>/i)?.[1]?.trim()
    : parseExtensionName(xml);
  if (!name) {
    throw new Error("Не удалось прочитать имя конфигурации или расширения из исходников.");
  }
  return { name, path: root };
}

async function discoverConfigurationRoots(root) {
  const candidates = [];
  const directories = [root];
  while (directories.length) {
    const directory = directories.pop();
    const entries = await fs.promises.readdir(directory, { withFileTypes: true }).catch(() => []);
    if (entries.some((entry) => entry.isFile() && entry.name === "Configuration.xml")
      || (entries.some((entry) => entry.isDirectory() && entry.name === "DT-INF")
        && entries.some((entry) => entry.isFile() && entry.name === ".project"))) {
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

async function pickIbasesFile(defaultUri) {
  const selected = await vscode.window.showOpenDialog({
    title: "1C: Выберите файл ibases.v8i",
    defaultUri,
    canSelectFiles: true,
    canSelectFolders: false,
    canSelectMany: false,
    openLabel: "Выбрать ibases.v8i",
    filters: { "Списки баз 1С": ["v8i"] }
  });
  return selected?.[0]?.fsPath;
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
        description: "Configuration.xml или EDT-проект",
        browse: true
      }
    ],
    {
      title: "1C: Исходники основной конфигурации",
      placeHolder: "Выберите каталог с Configuration.xml или EDT-проектом"
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

async function chooseExtensionsManually(workspaceFolder, baseProject) {
  const discovered = await discoveredExtensionCandidates(workspaceFolder, baseProject);
  let selectedPaths = [];
  while (true) {
    // A multi-select QuickPick cannot be accepted without a checked item.  When
    // no extensions were found, use a regular QuickPick with an explicit
    // continuation item so keyboard and accessibility-tool users can advance.
    const hasOnlyExternalChoices = discovered.length === 0 && selectedPaths.length === 0;
    const knownByPath = new Map(discovered.map((item) => [item.path, item]));
    const selected = await vscode.window.showQuickPick(
      hasOnlyExternalChoices
        ? noExtensionSourceChoices()
        : [
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
            description: "Каждый каталог должен быть XML-выгрузкой или EDT-проектом",
            browse: true
          }
        ],
      {
        title: "1C: Исходники расширений",
        placeHolder: hasOnlyExternalChoices
          ? "Расширения не найдены в рабочей области"
          : "Необязательно: выберите расширения, которые нужно отлаживать",
        canPickMany: !hasOnlyExternalChoices
      }
    );
    if (!selected) {
      return undefined;
    }
    if (!Array.isArray(selected) && selected.continueWithoutExtensions) {
      return [];
    }
    const selectedItems = (Array.isArray(selected) ? selected : [selected])
      .filter((item) => !item.browse);
    selectedPaths = selectedItems.map((item) => item.path);
    const isBrowseSelected = (Array.isArray(selected) ? selected : [selected])
      .some((item) => item.browse);
    if (isBrowseSelected) {
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
      selectedPaths = [];
    }
  }
}

function sameExtensionName(first, second) {
  return typeof first === "string" && typeof second === "string"
    && first.localeCompare(second, undefined, { sensitivity: "accent" }) === 0;
}

async function chooseExtensionSource(workspaceFolder, extensionName, candidates) {
  const matching = candidates.filter((item) => sameExtensionName(item.name, extensionName));
  while (true) {
    const picked = await vscode.window.showQuickPick(
      [
        ...matching.map((extension, index) => ({
          label: `$(folder) ${formatPathForWorkspace(extension.path, workspaceFolder)}`,
          description: index === 0 ? "Найдено совпадение по имени расширения" : "Ещё один каталог с таким именем",
          detail: extension.path,
          extension
        })),
        ...candidates
          .filter((extension) => !sameExtensionName(extension.name, extensionName))
          .map((extension) => ({
            label: `$(folder) ${formatPathForWorkspace(extension.path, workspaceFolder)}`,
            description: `Исходники «${extension.name}» — имя не совпадает`,
            detail: extension.path,
            extension
          })),
        {
          label: "$(folder-opened) Выбрать каталог…",
          description: "Каталог должен быть XML-выгрузкой или EDT-проектом",
          browse: true
        },
        {
          label: "Пропустить это расширение",
          description: "Точки останова в его исходниках не будут сопоставляться",
          skip: true
        }
      ],
      {
        title: `1C: Исходники расширения «${extensionName}»`,
        placeHolder: "Выберите соответствующий каталог исходников"
      }
    );
    // Esc cancels the complete wizard.  That is distinct from the explicit
    // per-extension skip, which keeps configuring the remaining extensions.
    if (!picked) return { cancelled: true };
    if (picked.skip) return { skipped: true };
    let selected = picked.extension;
    if (picked.browse) {
      const directory = (await pickDirectory(
        `1C: Исходники расширения «${extensionName}»`,
        workspaceFolder.uri
      ))?.[0];
      if (!directory) continue;
      try {
        selected = await extensionRoot(directory);
      } catch (error) {
        await vscode.window.showErrorMessage(`Не удалось добавить расширение: ${error.message}`);
        continue;
      }
    }
    if (!sameExtensionName(selected.name, extensionName)) {
      await vscode.window.showErrorMessage(
        `Выбран каталог расширения «${selected.name}», а в базе включено «${extensionName}». Выберите соответствующие исходники.`
      );
      continue;
    }
    return { extension: selected };
  }
}

async function chooseExtensionsFromInfoBase(workspaceFolder, baseProject, extensionNames) {
  const candidates = await discoveredExtensionCandidates(workspaceFolder, baseProject);
  const selected = [];
  for (const extensionName of extensionNames) {
    const result = await chooseExtensionSource(workspaceFolder, extensionName, candidates);
    if (result.cancelled) return undefined;
    const extension = result.extension;
    if (extension && !selected.some((item) => isSamePath(item.path, extension.path))) {
      selected.push(extension);
    }
  }
  return selected;
}

async function chooseRequest(selectedInfoBase) {
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
    {
      title: "1C: Режим отладки"
    }
  );
  return selected?.request;
}

async function chooseLaunchMode(selectedInfoBase, request) {
  if (request !== "launch") return "client";
  if (!selectedInfoBase.supportsStandaloneServer) return "client";
  const selected = await vscode.window.showQuickPick(
    [
      {
        label: "Обычный клиент 1С (рекомендуется)",
        description: "Запустить 1cv8c; для файловой базы адаптер сам поднимет временный dbgs",
        mode: "client"
      },
      {
        label: "Автономный сервер 1С (ibsrv)",
        description: "Запустить локальный ibsrv; отладчик создаст временный dbgs",
        detail: "После старта сервера откроется тонкий клиент; сервер остановится вместе с отладочной сессией.",
        mode: "standaloneServer"
      }
    ],
    {
      title: "1C: Способ запуска файловой базы",
      placeHolder: "Выберите, кто будет запускать базу"
    }
  );
  return selected?.mode;
}

async function chooseStandaloneTransport(launchMode) {
  if (launchMode !== "standaloneServer") return undefined;
  const selected = await vscode.window.showQuickPick(
    [
      {
        label: "Прямое соединение тонкого клиента (рекомендуется)",
        description: "TCP/IP через шлюз ibsrv; не использует HTTP-страницу входа",
        detail: "Тонкий клиент подключится к локальному автономному серверу напрямую.",
        transport: "direct"
      },
      {
        label: "HTTP-соединение тонкого клиента",
        description: "Тонкий клиент через /WS и встроенный HTTP-шлюз ibsrv",
        detail: "Нужно, только если требуется проверить именно HTTP-публикацию.",
        transport: "http"
      }
    ],
    {
      title: "1C: Подключение тонкого клиента к автономному серверу",
      placeHolder: "Выберите транспорт клиента"
    }
  );
  return selected?.transport;
}

async function firstAvailableLocalPort(first = 8314, last = 8399) {
  for (let port = first; port <= last; port += 1) {
    const available = await new Promise((resolve) => {
      const server = net.createServer();
      server.once("error", () => resolve(false));
      server.listen({ host: "127.0.0.1", port }, () => {
        server.close(() => resolve(true));
      });
    });
    if (available) return port;
  }
  return undefined;
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

function ibaseChoiceDescription(entry) {
  if (entry.kind === "file") return `Файловая: ${entry.filePath}`;
  if (entry.kind === "server") return `Серверная: ${entry.server}/${entry.reference}`;
  return "Зарегистрированная информационная база";
}

async function chooseInfoBase(workspaceFolder) {
  const [launcherEntries, projectEntries] = await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Window, title: "1C: Поиск зарегистрированных информационных баз" },
    () => Promise.all([
      discoverIbaseEntries(),
      discoverV8ProjectEntries(workspaceFolder.uri.fsPath)
    ])
  );
  let entries = mergeInfoBaseEntries(projectEntries, launcherEntries);
  while (true) {
    const selected = await vscode.window.showQuickPick(
      [
        ...entries.map((entry) => ({
          label: entry.name,
          description: ibaseChoiceDescription(entry),
          detail: entry.hasStoredCredentials
            ? "Сохранённые учётные данные останутся только в списке запуска 1С"
            : entry.source === "v8-project"
              ? `из .v8-project.json${entry.isDefault ? " — база по умолчанию" : ""}`
              : undefined,
          picked: entry.isDefault === true,
          entry
        })),
        ibasesFilePickerChoice(),
        {
          label: "$(folder-opened) Выбрать каталог файловой базы…",
          description: "База, которой нет в списке запуска 1С",
          browse: true
        },
        {
          label: "$(edit) Ввести серверное подключение…",
          description: "Формат: /Sserver\\base без учётных данных",
          manual: true
        }
      ],
      {
        title: "1C: Информационная база для отладки",
        placeHolder: "Выберите базу из списка запуска 1С"
      }
    );
    if (!selected) return undefined;
    if (selected.ibasesFile) {
      const ibasesFile = await pickIbasesFile(workspaceFolder.uri);
      if (!ibasesFile) continue;
      const imported = await vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Window,
          title: "1C: Чтение списка информационных баз"
        },
        () => discoverIbaseEntries({ files: [ibasesFile] })
      );
      if (!imported.length) {
        await vscode.window.showWarningMessage(
          "В выбранном файле ibases.v8i не найдено доступных подключений к информационным базам."
        );
        continue;
      }
      entries = mergeInfoBaseEntries(entries, imported);
      continue;
    }
    if (selected.entry) {
      if (selected.entry.kind === "file" && selected.entry.filePath) {
        return {
          infoBase: fileInfoBaseArgument(selected.entry.filePath),
          inventoryConnection: { kind: "file", value: selected.entry.filePath },
          supportsStandaloneServer: true
        };
      }
      if (selected.entry.kind === "server" && selected.entry.server && selected.entry.reference) {
        const connection = `${selected.entry.server}\\${selected.entry.reference}`;
        return {
          infoBase: serverInfoBaseArgument(selected.entry.server, selected.entry.reference),
          infoBaseAlias: selected.entry.reference,
          inventoryConnection: { kind: "server", value: connection },
          supportsStandaloneServer: false
        };
      }
      await vscode.window.showErrorMessage(
        "У выбранной записи нет файлового пути или пары Srvr/Ref; её нельзя записать в launch.json без имени регистрации."
      );
      continue;
    }
    if (selected.manual) {
      const infoBase = await inputInfoBase(
        "1C: Серверное подключение",
        "Введите /Sserver\\base без логина и пароля"
      );
      if (!infoBase) return undefined;
      const server = parseServerInfoBaseArgument(infoBase);
      if (!server) {
        await vscode.window.showErrorMessage(
          "Нужно указать сервер и базу в формате /Sserver\\base."
        );
        continue;
      }
      return {
        infoBase: serverInfoBaseArgument(server.server, server.reference),
        infoBaseAlias: server.reference,
        inventoryConnection: { kind: "server", value: `${server.server}\\${server.reference}` },
        supportsStandaloneServer: false
      };
    }
    const directory = (await pickDirectory("1C: Каталог файловой информационной базы", workspaceFolder.uri))?.[0];
    if (!directory) continue;
    try {
      const infoBase = await canonicalDirectory(directory);
      return {
        infoBase: fileInfoBaseArgument(infoBase),
        inventoryConnection: { kind: "file", value: infoBase },
        supportsStandaloneServer: true
      };
    } catch (error) {
      await vscode.window.showErrorMessage(`Не удалось выбрать файловую базу: ${error.message}`);
    }
  }
}

async function choosePlatformDirectory(workspaceFolder) {
  const discovered = await discoverPlatformDirectories();
  const choices = [
    ...discovered.map((directory, index) => ({
      label: `$(tools) ${platformVersionLabel(directory)}`,
      description: index === 0
        ? `Рекомендуется: ${directory}`
        : directory,
      detail: "Содержит 1cv8c и dbgs",
      directory
    })),
    {
      label: "$(folder-opened) Выбрать другой каталог…",
      description: "Для запуска нужны 1cv8c и dbgs",
      browse: true
    }
  ];
  const defaultRoot = defaultPlatformRoots()[0];
  while (true) {
    const picked = await vscode.window.showQuickPick(choices, {
      title: "1C: Платформа 1С для запуска",
      placeHolder: "Выберите каталог, содержащий 1cv8c и dbgs"
    });
    if (!picked) return undefined;
    const selected = picked.browse
      ? await pickDirectory(
        "1C: Каталог платформы 1С",
        defaultRoot ? vscode.Uri.file(defaultRoot) : workspaceFolder.uri
      )
      : [picked.directory];
    if (!selected) return undefined;
    try {
      return await validatePlatformDirectory(selected[0]);
    } catch (error) {
      await vscode.window.showErrorMessage(`Неверный каталог платформы: ${error.message}`);
    }
  }
}

function platformVersionLabel(directory) {
  const last = path.basename(directory);
  return last.toLocaleLowerCase() === "bin" ? path.basename(path.dirname(directory)) : last;
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

async function chooseLaunchCredentials(request) {
  if (request !== "launch") return { userName: undefined };
  const choice = await vscode.window.showQuickPick(
    [
      {
        label: "Без авторизации",
        description: "Клиент покажет обычный вход, если база его требует"
      },
      {
        label: "Указать пользователя и спрашивать пароль при запуске",
        description: "Пароль не записывается в launch.json",
        credentials: true
      }
    ],
    {
      title: "1C: Авторизация при запуске",
      placeHolder: "Выберите способ входа в базу"
    }
  );
  if (!choice) return undefined;
  if (!choice.credentials) return { userName: undefined };
  const userName = await vscode.window.showInputBox({
    title: "1C: Пользователь базы",
    prompt: "Имя пользователя будет передано тонкому клиенту как /N",
    placeHolder: "Например, Администратор",
    validateInput: (value) => value.trim() ? undefined : "Укажите имя пользователя."
  });
  return userName === undefined ? undefined : { userName: userName.trim() };
}

function passwordInputId(inputs) {
  const used = new Set(
    inputs
      .filter((input) => input && typeof input.id === "string")
      .map((input) => input.id)
  );
  let suffix = 1;
  let id = "onec.debugger.password";
  while (used.has(id)) {
    suffix += 1;
    id = `onec.debugger.password.${suffix}`;
  }
  return id;
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
  // The platform comes first: it is needed by Designer to inspect the chosen
  // base's enabled extensions, even when the final configuration is attach.
  const selectedPlatformPath = await choosePlatformDirectory(workspaceFolder);
  if (!selectedPlatformPath) return;
  const selectedInfoBase = await chooseInfoBase(workspaceFolder);
  if (!selectedInfoBase) return;
  const rootProject = await chooseBaseProject(workspaceFolder);
  if (!rootProject) return;

  let installedExtensionNames;
  try {
    installedExtensionNames = await vscode.window.withProgress(
      {
        location: vscode.ProgressLocation.Notification,
        title: "1C: Чтение списка расширений информационной базы",
        cancellable: false
      },
      () => discoverInfoBaseExtensions({
        platformDirectory: selectedPlatformPath,
        connection: selectedInfoBase.inventoryConnection
      })
    );
  } catch (error) {
    await vscode.window.showWarningMessage(
      `Не удалось автоматически прочитать расширения базы. Можно выбрать исходники вручную. Причина: ${error.message}`
    );
  }
  const extensions = Array.isArray(installedExtensionNames)
    ? await chooseExtensionsFromInfoBase(workspaceFolder, rootProject, installedExtensionNames)
    : await chooseExtensionsManually(workspaceFolder, rootProject);
  if (!extensions) return;

  const request = await chooseRequest(selectedInfoBase);
  if (!request) return;
  const credentials = await chooseLaunchCredentials(request);
  if (!credentials) return;
  const launchMode = await chooseLaunchMode(selectedInfoBase, request);
  if (!launchMode) return;
  const standaloneTransport = await chooseStandaloneTransport(launchMode);
  if (launchMode === "standaloneServer" && !standaloneTransport) return;

  const debugServer = await chooseDebugServer();
  if (!debugServer) return;
  const aliasResult = request === "attach" ? await chooseOptionalAlias() : { value: undefined };
  if (!aliasResult) return;

  const existing = await launchConfigurationsFor(workspaceFolder);
  const existingInputs = vscode.workspace
    .getConfiguration("launch", workspaceFolder.uri)
    .get("inputs", []);
  const inputs = Array.isArray(existingInputs) ? existingInputs : [];
  const mode = request === "launch" ? "запуск" : "подключение";
  const configuration = {
    name: uniqueConfigurationName(existing, `1C: ${path.basename(rootProject)} (${mode})`),
    type: "onec",
    request,
    launchMode,
    rootProject,
    infoBase: selectedInfoBase.infoBase,
    debugServerHost: debugServer.host,
    debugServerPort: debugServer.port,
    autoAttachTypes: ["ManagedClient", "Server"]
  };
  if (selectedInfoBase.infoBaseAlias) configuration.infoBaseAlias = selectedInfoBase.infoBaseAlias;
  if (aliasResult.value) configuration.infoBaseAlias = aliasResult.value;
  if (extensions.length) configuration.extensions = extensions.map((extension) => extension.path);
  let passwordInput;
  if (credentials.userName) {
    const inputId = passwordInputId(inputs);
    configuration.userName = credentials.userName;
    configuration.password = `\${input:${inputId}}`;
    passwordInput = {
      id: inputId,
      type: "promptString",
      description: `Пароль пользователя «${credentials.userName}» для 1С`,
      password: true
    };
  }
  if (request === "launch") {
    configuration.platformPath = selectedPlatformPath;
    configuration.platformVersion = "LATEST";
    if (launchMode === "standaloneServer") {
      const standaloneServerPort = await firstAvailableLocalPort();
      if (!standaloneServerPort) {
        await vscode.window.showErrorMessage("Не найден свободный порт для HTTP автономного сервера (8314–8399).");
        return;
      }
      // `ibsrv` binds a single address. A numeric loopback avoids a macOS
      // IPv6 `localhost` lookup that the thin client may not fall back from.
      configuration.standaloneServerHost = "127.0.0.1";
      configuration.standaloneServerPort = standaloneServerPort;
      configuration.standaloneServerBase = "/";
      configuration.standaloneServerTransport = standaloneTransport;
      configuration.standaloneServerDirectRegPort = 1941;
      configuration.standaloneServerDirectRange = "1960:1991";
      configuration.standaloneServerName = `onec-debug-${configuration.standaloneServerDirectRegPort}`;
      configuration.standaloneServerDataPath = path.join(
        workspaceFolder.uri.fsPath,
        ".vscode",
        "onec-standalone-server"
      );
    }
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
    const launchSettings = vscode.workspace.getConfiguration("launch", workspaceFolder.uri);
    if (passwordInput) {
      await launchSettings.update(
        "inputs",
        [...inputs, passwordInput],
        vscode.ConfigurationTarget.WorkspaceFolder
      );
    }
    await launchSettings.update(
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

class PerformanceMeasurementsProvider {
  constructor() {
    this.items = [];
    this.changeEmitter = new vscode.EventEmitter();
    this.onDidChangeTreeData = this.changeEmitter.event;
  }

  update(items) {
    this.items = Array.isArray(items) ? items : [];
    this.changeEmitter.fire();
  }

  getTreeItem(item) {
    const duration = Number(item.duration || 0).toFixed(3);
    const pureDuration = Number(item.pureDuration || 0).toFixed(3);
    const label = `${path.basename(item.source?.path || "модуль")}:${item.line}`;
    const treeItem = new vscode.TreeItem(label, vscode.TreeItemCollapsibleState.None);
    treeItem.description = `время ${duration}, чистое ${pureDuration}, ${item.frequency || 0} выз.`;
    treeItem.tooltip = [
      item.source?.path || "",
      `Строка: ${item.line}`,
      `Вызовы: ${item.frequency || 0}`,
      `Общее время: ${duration} (единицы платформы)`,
      `Чистое время: ${pureDuration} (единицы платформы)`,
      `Сигнал серверного вызова: ${item.serverCallSignal || 0}`
    ].join("\n");
    treeItem.command = item.source?.path
      ? {
        command: "vscode.open",
        title: "Открыть измеренную строку",
        arguments: [vscode.Uri.file(item.source.path), { selection: new vscode.Range(item.line - 1, 0, item.line - 1, 0) }]
      }
      : undefined;
    return treeItem;
  }

  getChildren(element) {
    return element ? [] : this.items;
  }
}

const performanceMeasurementsProvider = new PerformanceMeasurementsProvider();
const performanceDecorations = new Map();

function clearPerformanceDecorations() {
  for (const decoration of performanceDecorations.values()) {
    decoration.dispose();
  }
  performanceDecorations.clear();
}

function showPerformanceDecorations(items) {
  clearPerformanceDecorations();
  const bySource = new Map();
  for (const item of items) {
    const source = item?.source?.path;
    if (typeof source !== "string" || !Number.isInteger(item.line) || item.line < 1) continue;
    const values = bySource.get(source) || [];
    values.push(item);
    bySource.set(source, values);
  }
  for (const [source, sourceItems] of bySource) {
    const decoration = vscode.window.createTextEditorDecorationType({
      after: { color: new vscode.ThemeColor("editorCodeLens.foreground"), margin: "0 0 0 2em" },
      isWholeLine: true
    });
    performanceDecorations.set(source, decoration);
    for (const editor of vscode.window.visibleTextEditors) {
      if (editor.document.uri.fsPath !== source) continue;
      editor.setDecorations(decoration, sourceItems.map((item) => ({
        range: new vscode.Range(item.line - 1, 0, item.line - 1, 0),
        renderOptions: {
          after: {
            contentText: `$(pulse) ${Number(item.duration || 0).toFixed(3)} · ${item.frequency || 0} выз.`
          }
        },
        hoverMessage: new vscode.MarkdownString(
          `**Замер 1С**  \nОбщее: ${Number(item.duration || 0).toFixed(3)} (единицы платформы)  \nЧистое: ${Number(item.pureDuration || 0).toFixed(3)} (единицы платформы)  \nВызовы: ${item.frequency || 0}  \nСигнал серверного вызова: ${item.serverCallSignal || 0}`
        )
      })));
    }
  }
}

function updatePerformanceMeasurements(session) {
  if (session?.type !== "onec") return Promise.resolve();
  return session
    .customRequest("PerformanceMeasurementResultsRequest")
    .then((body) => {
      const results = body?.results || [];
      performanceMeasurementsProvider.update(results);
      showPerformanceDecorations(results);
    })
    .catch(() => undefined);
}

async function startPerformanceMeasurement() {
  const session = activeOnecDebugSession();
  if (!session) {
    await vscode.window.showErrorMessage("Сначала запустите отладку 1С и дождитесь цели отладки.");
    return;
  }
  const response = await session.customRequest("threads");
  const threads = Array.isArray(response?.threads) ? response.threads : [];
  if (threads.length === 0) {
    await vscode.window.showErrorMessage("Нет подключённых целей 1С. Выполните действие в тонком клиенте и подключите цель отладки.");
    return;
  }
  const selected = await vscode.window.showQuickPick(
    threads.map((thread) => ({ label: thread.name, description: `поток ${thread.id}`, threadId: thread.id })),
    {
      title: "1C: Начать замер производительности",
      placeHolder: "Выберите активную цель 1С (платформа может вернуть результаты по нескольким целям)"
    }
  );
  if (!selected) return;
  try {
    await session.customRequest("StartPerformanceMeasurementRequest", { threadId: selected.threadId });
  } catch (error) {
    await vscode.window.showErrorMessage(`Не удалось начать замер производительности: ${error.message || error}`);
    return;
  }
  performanceMeasurementsProvider.update([]);
  clearPerformanceDecorations();
  await vscode.window.showInformationMessage("Замер производительности 1С запущен.");
}

async function stopPerformanceMeasurement() {
  const session = activeOnecDebugSession();
  if (!session) return;
  try {
    await session.customRequest("StopPerformanceMeasurementRequest");
  } catch (error) {
    await vscode.window.showErrorMessage(`Не удалось остановить замер производительности: ${error.message || error}`);
    return;
  }
  await vscode.window.showInformationMessage("Замер остановлен; ожидаю результаты от платформы 1С.");
}

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
    vscode.window.registerTreeDataProvider("debug.performanceMeasurements", performanceMeasurementsProvider)
  );
  context.subscriptions.push(
    vscode.commands.registerCommand("onec.performance.start", startPerformanceMeasurement)
  );
  context.subscriptions.push(
    vscode.commands.registerCommand("onec.performance.stop", stopPerformanceMeasurement)
  );
  context.subscriptions.push(
    vscode.commands.registerCommand("onec.performance.clear", () => {
      performanceMeasurementsProvider.update([]);
      clearPerformanceDecorations();
    })
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
        performanceMeasurementsProvider.update([]);
        clearPerformanceDecorations();
      }
    })
  );
  context.subscriptions.push(
    vscode.window.onDidChangeVisibleTextEditors(() => {
      showPerformanceDecorations(performanceMeasurementsProvider.items);
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
      if (
        debugEvent.session.type === "onec" &&
        debugEvent.event === "PerformanceMeasurementUpdated"
      ) {
        updatePerformanceMeasurements(debugEvent.session);
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
  PerformanceMeasurementsProvider,
  performanceMeasurementsProvider,
  updatePerformanceMeasurements,
  clearPerformanceDecorations,
  isPlatformVersionDirectory,
  validatePlatformDirectory
};
