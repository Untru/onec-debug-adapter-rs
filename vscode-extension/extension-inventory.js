const childProcess = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { hasCredentials } = require("./setup-wizard");

const DEFAULT_TIMEOUT_MS = 45_000;

function executableName(platform = process.platform) {
  return platform === "win32" ? "1cv8.exe" : "1cv8";
}

async function designerExecutable(platformDirectory, options = {}) {
  const platform = options.platform ?? process.platform;
  const fileSystem = options.fileSystem ?? fs.promises;
  const name = executableName(platform);
  const candidates = [
    path.join(platformDirectory, name),
    // The setup wizard normally stores the bin directory on Windows.  This
    // second form also makes the helper safe for a caller that passed the
    // version directory itself.
    path.join(platformDirectory, "bin", name)
  ];
  for (const candidate of candidates) {
    const stat = await fileSystem.stat(candidate).catch(() => undefined);
    if (stat?.isFile()) return candidate;
  }
  throw new Error("Не найден исполняемый файл 1cv8 для запуска Конфигуратора.");
}

function validateConnection(connection) {
  if (!connection || (connection.kind !== "file" && connection.kind !== "registered")) {
    throw new Error("Для чтения расширений укажите файловую или зарегистрированную информационную базу.");
  }
  if (typeof connection.value !== "string" || !connection.value.trim()) {
    throw new Error("Не указана информационная база.");
  }
  if (hasCredentials(connection.value)) {
    throw new Error("Не указывайте учётные данные при чтении расширений.");
  }
  return { kind: connection.kind, value: connection.value.trim() };
}

function designerArguments(connection, resultFile) {
  const checked = validateConnection(connection);
  return [
    "DESIGNER",
    checked.kind === "file" ? "/F" : "/IBName",
    checked.value,
    "/DisableStartupMessages",
    "/DumpDBCfgList",
    "-AllExtensions",
    "/Out",
    resultFile
  ];
}

function decodeDesignerOutput(contents) {
  if (!Buffer.isBuffer(contents)) return String(contents ?? "");
  if (contents.length >= 2 && contents[0] === 0xff && contents[1] === 0xfe) {
    return contents.subarray(2).toString("utf16le");
  }
  return contents.toString("utf8").replace(/^\uFEFF/, "");
}

function isResultNoise(line) {
  return /^(?:information\s+for\s+technical\s+support|information|warning|error|configuration\s+name|configuration\s+extensions?|информация(?:\s+для\s+технической\s+поддержки)?|предупреждение|ошибка|имя\s+конфигурации|расширени(?:е|я)\s+конфигурации|список\s+расширений\s+конфигурации)\s*:?$/i.test(line);
}

function normalizeListedName(line) {
  const labelled = line.match(
    /^(?:configuration\s+extension|extension|расширение\s+конфигурации|расширение)\s*[:=-]\s*(.+)$/i
  );
  const value = (labelled?.[1] ?? line).replace(/^(?:[-*•]|\d+[.)])\s*/, "").trim();
  if (!value || isResultNoise(value)) return undefined;
  // Names of 1C metadata objects never contain an angle bracket or a line
  // break.  This rejects common status/log fragments without restricting
  // localized or space-containing extension names.
  if (/[<>\r\n]/.test(value) || value.length > 255) return undefined;
  return value;
}

/**
 * Parse the human-readable result of Designer's /DumpDBCfgList command.
 * The platform writes one extension name per line in supported 8.3 releases;
 * labels and bullets vary by localization, so both forms are accepted.
 */
function parseDesignerExtensionList(contents) {
  const names = [];
  const seen = new Set();
  for (const line of decodeDesignerOutput(contents).split(/\r?\n/)) {
    const name = normalizeListedName(line.trim());
    if (!name) continue;
    const key = name.toLocaleLowerCase();
    if (!seen.has(key)) {
      seen.add(key);
      names.push(name);
    }
  }
  return names;
}

function run(executable, args, options = {}) {
  const spawn = options.spawn ?? childProcess.spawn;
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  return new Promise((resolve, reject) => {
    let child;
    try {
      child = spawn(executable, args, { shell: false, windowsHide: true, stdio: "ignore" });
    } catch (error) {
      reject(error);
      return;
    }
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill();
    }, timeoutMs);
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once("close", (code) => {
      clearTimeout(timer);
      if (timedOut) {
        reject(new Error("Конфигуратор не ответил при чтении списка расширений."));
      } else if (code !== 0) {
        reject(new Error("Не удалось прочитать список расширений из информационной базы."));
      } else {
        resolve();
      }
    });
  });
}

/**
 * Reads extension names from an infobase without modifying it.  The only
 * artifact is a private temporary Designer result file, removed in finally.
 * The caller receives only extension names, never command output or secrets.
 */
async function discoverInfoBaseExtensions(options) {
  const fileSystem = options.fileSystem ?? fs.promises;
  const tempRoot = options.tempRoot ?? os.tmpdir();
  const temporary = await fileSystem.mkdtemp(path.join(tempRoot, "onec-extension-list-"));
  const outputFile = path.join(temporary, "designer-result.txt");
  try {
    const executable = options.executable ?? await designerExecutable(options.platformDirectory, options);
    await (options.run ?? run)(executable, designerArguments(options.connection, outputFile), options);
    const output = await fileSystem.readFile(outputFile).catch(() => Buffer.alloc(0));
    return parseDesignerExtensionList(output);
  } finally {
    await fileSystem.rm(temporary, { recursive: true, force: true }).catch(() => undefined);
  }
}

module.exports = {
  DEFAULT_TIMEOUT_MS,
  decodeDesignerOutput,
  designerArguments,
  designerExecutable,
  discoverInfoBaseExtensions,
  executableName,
  parseDesignerExtensionList,
  validateConnection
};
