const path = require("node:path");
const fs = require("node:fs");

const CREDENTIAL_PATTERN = /(?:^|[;\s])(pwd|password|usr|user)\s*=/i;

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
  if (platform === "darwin" || platform === "linux") return ["/opt/1cv8"];
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
  configurationSummary,
  containsPlatformBinaries,
  defaultPlatformRoots,
  discoverPlatformDirectories,
  hasCredentials,
  isSamePath,
  isMacApplicationBundle,
  isPlatformVersionDirectory,
  noExtensionSourceChoices,
  parseExtensionName,
  platformBinaryDirectory,
  platformExecutables,
  uniqueConfigurationName,
  validatePlatformDirectory
};
