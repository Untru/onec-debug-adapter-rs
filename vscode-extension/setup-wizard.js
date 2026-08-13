const path = require("node:path");
const fs = require("node:fs");

const CREDENTIAL_PATTERN = /(?:^|[;\s])(pwd|password|usr|user)\s*=/i;

// `ibases.v8i` is an INI-like file maintained by the 1C launcher.  It is not
// a credentials store for the wizard: only safe identity fields are exposed.
const IBASE_SECTION_PATTERN = /^\s*\[([^\]]+)\]\s*$/;

function parseExtensionName(xml) {
  if (typeof xml !== "string") {
    return undefined;
  }
  const properties = xml.match(/<Properties\b[^>]*>([\s\S]*?)<\/Properties>/i);
  const scope = properties ? properties[1] : xml;
  const name = scope.match(/<Name\b[^>]*>\s*([^<]+?)\s*<\/Name>/i);
  return name ? decodeXmlText(name[1]).trim() || undefined : undefined;
}

function decodeXmlText(value) {
  return value.replace(/&(lt|gt|amp|quot|apos);/gi, (_match, entity) => {
    const entities = { lt: "<", gt: ">", amp: "&", quot: "\"", apos: "'" };
    return entities[entity.toLowerCase()];
  });
}

function hasCredentials(value) {
  return typeof value === "string" && CREDENTIAL_PATTERN.test(value);
}

function connectionProperty(connection, property) {
  if (typeof connection !== "string") return undefined;
  const expected = property.toLocaleLowerCase();
  for (const part of connection.split(";")) {
    const separator = part.indexOf("=");
    if (separator < 0) continue;
    if (part.slice(0, separator).trim().toLocaleLowerCase() !== expected) continue;
    const value = part.slice(separator + 1).trim();
    return value.replace(/^"(.*)"$/, "$1").trim() || undefined;
  }
  return undefined;
}

function parseIbaseV8i(contents) {
  if (typeof contents !== "string") return [];
  const entries = [];
  let name;
  let connect;
  const finish = () => {
    if (!name || !connect) return;
    const filePath = connectionProperty(connect, "File");
    const server = connectionProperty(connect, "Srvr");
    const reference = connectionProperty(connect, "Ref");
    entries.push({
      name,
      kind: filePath ? "file" : (server && reference ? "server" : "unknown"),
      filePath,
      server,
      reference,
      // This is only a UI hint.  No user name, password or connection string
      // leaves this parser; launch.json will contain the registration name.
      hasStoredCredentials: hasCredentials(connect)
    });
  };
  for (const rawLine of contents.split(/\r?\n/)) {
    const section = rawLine.match(IBASE_SECTION_PATTERN);
    if (section) {
      finish();
      name = section[1].trim();
      connect = undefined;
      continue;
    }
    if (!name) continue;
    const separator = rawLine.indexOf("=");
    if (separator < 0) continue;
    if (rawLine.slice(0, separator).trim().toLocaleLowerCase() === "connect") {
      connect = rawLine.slice(separator + 1).trim();
    }
  }
  finish();
  return entries;
}

function defaultIbaseDirectories(platform = process.platform, environment = process.env) {
  if (platform === "win32") {
    return [environment.APPDATA, environment.LOCALAPPDATA]
      .filter(Boolean)
      .map((directory) => path.join(directory, "1C", "1CEStart"));
  }
  const home = environment.HOME;
  if (!home) return [];
  if (platform === "darwin") {
    return [
      path.join(home, ".1C", "1cestart"),
      path.join(home, "Library", "Application Support", "1C", "1CEStart"),
      path.join(home, ".1cv8", "1C", "1CEStart")
    ];
  }
  return [
    path.join(home, ".1C", "1cestart"),
    path.join(home, ".1cv8", "1C", "1CEStart")
  ];
}

function defaultIbaseFiles(platform = process.platform, environment = process.env) {
  return defaultIbaseDirectories(platform, environment)
    .map((directory) => path.join(directory, "ibases.v8i"));
}

function decodePlatformText(value) {
  if (typeof value === "string") return value.replace(/^\uFEFF/, "");
  if (!Buffer.isBuffer(value)) return undefined;
  if (value.length >= 2 && value[0] === 0xff && value[1] === 0xfe) {
    return value.subarray(2).toString("utf16le");
  }
  if (value.length >= 3 && value[0] === 0xef && value[1] === 0xbb && value[2] === 0xbf) {
    return value.subarray(3).toString("utf8");
  }
  return value.toString("utf8");
}

function commonInfoBaseListPaths(contents) {
  if (typeof contents !== "string") return [];
  const paths = [];
  for (const rawLine of contents.split(/\r?\n/)) {
    const line = rawLine.trim();
    const separator = line.indexOf("=");
    if (separator <= 0) continue;
    if (line.slice(0, separator).trim().toLocaleLowerCase() !== "commoninfobases") continue;
    const value = line.slice(separator + 1).trim();
    if (value) paths.push(...value.split(";").map((item) => item.trim()).filter(Boolean));
  }
  return paths;
}

async function readPlatformText(file, fileSystem = fs.promises) {
  const raw = await fileSystem.readFile(file).catch(() => undefined);
  return decodePlatformText(raw);
}

function uniquePaths(paths, platform = process.platform) {
  const seen = new Set();
  return paths.filter((file) => {
    const normalized = path.resolve(file);
    const key = platform === "win32" ? normalized.toLocaleLowerCase() : normalized;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function launcherEntryKey(entry, platform = process.platform) {
  const normalize = (value) => platform === "win32" ? value.toLocaleLowerCase() : value;
  if (entry.kind === "file" && entry.filePath) return `${normalize(entry.name)}|file:${normalize(entry.filePath)}`;
  if (entry.kind === "server" && entry.server && entry.reference) {
    return `${normalize(entry.name)}|server:${normalize(entry.server)}\\${normalize(entry.reference)}`;
  }
  return `${normalize(entry.name)}|unknown`;
}

async function discoverIbaseEntries(options = {}) {
  const platform = options.platform ?? process.platform;
  const fileSystem = options.fileSystem ?? fs.promises;
  const primaryFiles = options.files ?? defaultIbaseFiles(platform, options.environment);
  const startupDirectories = options.startupDirectories
    ?? (options.files ? primaryFiles.map((file) => path.dirname(file)) : defaultIbaseDirectories(platform, options.environment));
  const files = [...primaryFiles];
  for (const directory of uniquePaths(startupDirectories, platform)) {
    const config = await readPlatformText(path.join(directory, "1cestart.cfg"), fileSystem);
    if (!config) continue;
    files.push(...commonInfoBaseListPaths(config).map((file) => path.resolve(directory, file)));
  }
  const entries = [];
  const seen = new Set();
  for (const file of uniquePaths(files, platform)) {
    const contents = await readPlatformText(file, fileSystem);
    if (contents === undefined) continue;
    for (const entry of parseIbaseV8i(contents)) {
      const key = launcherEntryKey(entry, platform);
      if (seen.has(key)) continue;
      seen.add(key);
      entries.push({ ...entry, sourceFile: file });
    }
  }
  return entries.sort((first, second) => first.name.localeCompare(second.name));
}

function safeProjectText(value) {
  return typeof value === "string" && value.trim() && !hasCredentials(value)
    ? value.trim()
    : undefined;
}

function resolveProjectFilePath(projectFile, value) {
  const candidate = safeProjectText(value);
  if (!candidate) return undefined;
  return path.isAbsolute(candidate)
    ? path.normalize(candidate)
    : path.resolve(path.dirname(projectFile), candidate);
}

function projectDefaultId(project) {
  if (typeof project?.default === "string") return project.default.trim();
  if (project?.default && typeof project.default === "object") {
    return safeProjectText(project.default.id);
  }
  return undefined;
}

/**
 * Read only the connection identity fields supported by v8-project.json.
 * In particular, credentials and arbitrary connection-string properties are
 * intentionally neither parsed nor returned to the caller.
 */
function parseV8Project(projectFile, contents) {
  let project;
  try {
    project = typeof contents === "string" ? JSON.parse(contents) : contents;
  } catch {
    return [];
  }
  if (!project || typeof project !== "object" || !Array.isArray(project.databases)) return [];
  const defaultId = projectDefaultId(project);
  const entries = [];
  for (const database of project.databases) {
    if (!database || typeof database !== "object") continue;
    const id = safeProjectText(database.id);
    const name = safeProjectText(database.name) ?? id;
    const type = safeProjectText(database.type)?.toLocaleLowerCase();
    if (!id || !name || !["file", "server"].includes(type)) continue;
    if (type === "file") {
      const filePath = resolveProjectFilePath(projectFile, database.path);
      if (!filePath) continue;
      entries.push({
        id,
        name,
        kind: "file",
        filePath,
        isDefault: id === defaultId,
        source: "v8-project",
        sourceFile: projectFile
      });
      continue;
    }
    const server = safeProjectText(database.server);
    const reference = safeProjectText(database.ref);
    if (!server || !reference) continue;
    entries.push({
      id,
      name,
      kind: "server",
      server,
      reference,
      isDefault: id === defaultId,
      source: "v8-project",
      sourceFile: projectFile
    });
  }
  return entries;
}

async function discoverV8ProjectEntries(workspaceDirectory, options = {}) {
  const fileSystem = options.fileSystem ?? fs.promises;
  const projectFile = options.projectFile ?? path.join(workspaceDirectory, ".v8-project.json");
  const contents = await fileSystem.readFile(projectFile, "utf8").catch(() => undefined);
  return contents === undefined ? [] : parseV8Project(projectFile, contents);
}

function infoBaseIdentity(entry) {
  if (!entry || typeof entry !== "object") return undefined;
  const normalized = (value) => process.platform === "win32" ? value.toLocaleLowerCase() : value;
  if (entry.kind === "file" && typeof entry.filePath === "string") {
    return `file:${normalized(path.normalize(entry.filePath))}`;
  }
  if (entry.kind === "server" && typeof entry.server === "string" && typeof entry.reference === "string") {
    return `server:${normalized(entry.server)}\\${normalized(entry.reference)}`;
  }
  return typeof entry.name === "string" ? `registered:${normalized(entry.name)}` : undefined;
}

function mergeInfoBaseEntries(projectEntries, launcherEntries) {
  const merged = [];
  const seen = new Set();
  for (const entry of [...projectEntries, ...launcherEntries]) {
    const identity = infoBaseIdentity(entry);
    if (!identity || seen.has(identity)) continue;
    seen.add(identity);
    merged.push(entry);
  }
  return merged.sort((first, second) => {
    if (first.isDefault !== second.isDefault) return first.isDefault ? -1 : 1;
    return first.name.localeCompare(second.name);
  });
}

/**
 * A standard VS Code file picker works for an ibases.v8i stored in any
 * location (including a portable 1C installation).  Keep this item separate
 * from the directory picker so an empty workspace can still import the
 * launcher's registrations before it knows anything about the project.
 */
function ibasesFilePickerChoice() {
  return {
    label: "$(file) Выбрать файл ibases.v8i…",
    description: "Импортировать список баз из другого расположения",
    ibasesFile: true
  };
}

function uniqueConfigurationName(configurations, preferred) {
  const names = new Set(
    configurations
      .filter((item) => item && typeof item.name === "string")
      .map((item) => item.name)
  );
  if (!names.has(preferred)) {
    return preferred;
  }
  for (let number = 2; ; number += 1) {
    const candidate = `${preferred} (${number})`;
    if (!names.has(candidate)) {
      return candidate;
    }
  }
}

function isSamePath(first, second) {
  if (typeof first !== "string" || typeof second !== "string") {
    return false;
  }
  const normalize = process.platform === "win32"
    ? (value) => path.resolve(value).toLowerCase()
    : (value) => path.resolve(value);
  return normalize(first) === normalize(second);
}

function configurationSummary(configuration) {
  const lines = [
    `Название: ${configuration.name}`,
    `Режим: ${configuration.request === "launch" ? "запуск" : "подключение"}`,
    `Способ запуска: ${configuration.launchMode === "standaloneServer" ? "автономный сервер (ibsrv)" : "обычный клиент 1С"}`,
    `Исходники конфигурации: ${configuration.rootProject}`,
    `Информационная база: ${configuration.infoBase}`
  ];
  if (configuration.infoBaseAlias) {
    lines.push(`Псевдоним базы: ${configuration.infoBaseAlias}`);
  }
  if (configuration.request === "launch") {
    lines.push(`Платформа: ${configuration.platformPath}`);
    lines.push(`Версия платформы: ${configuration.platformVersion}`);
  }
  if (configuration.launchMode === "standaloneServer") {
    lines.push(`HTTP автономного сервера: ${configuration.standaloneServerHost}:${configuration.standaloneServerPort}${configuration.standaloneServerBase}`);
    const directPorts = `direct ${configuration.standaloneServerDirectRegPort} (${configuration.standaloneServerDirectRange})`;
    const sshPort = Number.isInteger(configuration.standaloneServerSshPort)
      ? `, SSH ${configuration.standaloneServerSshPort}`
      : "";
    lines.push(`Порты автономного сервера: ${directPorts}${sshPort}`);
    lines.push(`Транспорт тонкого клиента: ${configuration.standaloneServerTransport === "http" ? "HTTP (/WS)" : `прямой TCP/IP (${configuration.standaloneServerName || "имя сервера"})`}`);
    lines.push(`Данные автономного сервера: ${configuration.standaloneServerDataPath}`);
  }
  lines.push(`Сервер отладки: ${configuration.debugServerHost}:${configuration.debugServerPort}`);
  lines.push(`Автоподключение: ${configuration.autoAttachTypes.join(", ")}`);
  if (configuration.extensions?.length) {
    lines.push(`Расширения (${configuration.extensions.length}):`);
    lines.push(...configuration.extensions.map((item) => `  ${item}`));
  }
  return lines.join("\n");
}

function noExtensionSourceChoices() {
  return [
    {
      label: "Продолжить без расширений",
      description: "Настроить отладку только основной конфигурации",
      continueWithoutExtensions: true
    },
    {
      label: "$(folder-opened) Добавить каталоги вне рабочей области…",
      description: "Каждый каталог должен содержать Configuration.xml",
      browse: true
    }
  ];
}

function platformExecutables(platform = process.platform) {
  const suffix = platform === "win32" ? ".exe" : "";
  return { client: `1cv8c${suffix}`, debugServer: `dbgs${suffix}` };
}

function platformBinaryDirectory(versionDirectory, platform = process.platform) {
  return platform === "win32" ? path.join(versionDirectory, "bin") : versionDirectory;
}

function isPlatformVersionDirectory(name) {
  return name.split(".").every((part) => /^\d+$/.test(part));
}

function isMacApplicationBundle(directory) {
  const normalized = path.resolve(directory).replace(/\\/g, "/");
  return /\.app(?:\/Contents(?:\/MacOS)?)?$/.test(normalized);
}

async function canonicalDirectory(directory, fileSystem = fs.promises) {
  const canonical = await fileSystem.realpath(directory);
  const stat = await fileSystem.stat(canonical);
  if (!stat.isDirectory()) {
    throw new Error("Укажите каталог, а не файл.");
  }
  return canonical;
}

async function containsPlatformBinaries(directory, platform = process.platform, fileSystem = fs.promises) {
  const { client, debugServer } = platformExecutables(platform);
  const [clientStat, serverStat] = await Promise.all(
    [client, debugServer].map((name) => fileSystem.stat(path.join(directory, name)).catch(() => undefined))
  );
  return Boolean(clientStat?.isFile() && serverStat?.isFile());
}

function macApplicationBundleError() {
  return "Каталог 1cv8.app предназначен только для GUI-клиента и не содержит 1cv8c и dbgs. "
    + "Для запуска выберите /opt/1cv8/<версия> (например, /opt/1cv8/8.3.27.1508).";
}

async function validatePlatformDirectory(directory, options = {}) {
  const platform = options.platform ?? process.platform;
  const fileSystem = options.fileSystem ?? fs.promises;
  const root = await canonicalDirectory(directory, fileSystem);
  if (platform === "darwin" && isMacApplicationBundle(root)) {
    throw new Error(macApplicationBundleError());
  }
  if (await containsPlatformBinaries(root, platform, fileSystem)) return root;
  const children = await fileSystem.readdir(root, { withFileTypes: true });
  const candidates = await Promise.all(
    children
      .filter((entry) => entry.isDirectory() && isPlatformVersionDirectory(entry.name))
      .map(async (entry) => {
        const child = platformBinaryDirectory(path.join(root, entry.name), platform);
        return (await containsPlatformBinaries(child, platform, fileSystem)) ? child : undefined;
      })
  );
  if (candidates.some(Boolean)) return root;
  const { client, debugServer } = platformExecutables(platform);
  throw new Error(`Не найдены ${client} и ${debugServer} ни в каталоге, ни в его каталогах версий.`);
}

function defaultPlatformRoots(platform = process.platform) {
  if (platform === "darwin") return ["/opt/1cv8"];
  if (platform === "linux") {
    return ["/opt/1cv8", "/opt/1C/v8.3/x86_64", "/opt/1C/v8.3/i386"];
  }
  if (platform === "win32") {
    return [process.env["ProgramFiles"], process.env["ProgramFiles(x86)"]]
      .filter(Boolean)
      .map((directory) => path.join(directory, "1cv8"));
  }
  return [];
}

async function discoverPlatformDirectories(options = {}) {
  const platform = options.platform ?? process.platform;
  const fileSystem = options.fileSystem ?? fs.promises;
  const roots = options.roots ?? defaultPlatformRoots(platform);
  const discovered = [];
  for (const root of roots) {
    const directory = await canonicalDirectory(root, fileSystem).catch(() => undefined);
    if (!directory) continue;
    const entries = await fileSystem.readdir(directory, { withFileTypes: true }).catch(() => []);
    for (const entry of entries) {
      if (!entry.isDirectory() || !isPlatformVersionDirectory(entry.name)) continue;
      const candidate = platformBinaryDirectory(path.join(directory, entry.name), platform);
      if (await containsPlatformBinaries(candidate, platform, fileSystem)) discovered.push(candidate);
    }
  }
  return discovered.sort((first, second) => second.localeCompare(first, undefined, { numeric: true }));
}

module.exports = {
  canonicalDirectory,
  connectionProperty,
  configurationSummary,
  commonInfoBaseListPaths,
  decodePlatformText,
  defaultIbaseDirectories,
  defaultIbaseFiles,
  containsPlatformBinaries,
  defaultPlatformRoots,
  discoverIbaseEntries,
  discoverV8ProjectEntries,
  discoverPlatformDirectories,
  hasCredentials,
  infoBaseIdentity,
  ibasesFilePickerChoice,
  isSamePath,
  isMacApplicationBundle,
  isPlatformVersionDirectory,
  mergeInfoBaseEntries,
  noExtensionSourceChoices,
  parseExtensionName,
  parseIbaseV8i,
  parseV8Project,
  platformBinaryDirectory,
  platformExecutables,
  readPlatformText,
  uniqueConfigurationName,
  validatePlatformDirectory
};
