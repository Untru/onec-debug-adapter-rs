const path = require("node:path");

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

module.exports = {
  configurationSummary,
  hasCredentials,
  isSamePath,
  parseExtensionName,
  uniqueConfigurationName
};
