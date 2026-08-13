const childProcess = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const { hasCredentials } = require("./setup-wizard");

const DEFAULT_TIMEOUT_MS = 45_000;
const TERMINATE_GRACE_MS = 5_000;
const MAX_CAPTURED_OUTPUT_BYTES = 1_048_576;

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
  if (!connection || !["file", "registered", "server"].includes(connection.kind)) {
    throw new Error("Для чтения расширений укажите файловую, серверную или зарегистрированную информационную базу.");
  }
  if (typeof connection.value !== "string" || !connection.value.trim()) {
    throw new Error("Не указана информационная база.");
  }
  if (hasCredentials(connection.value)) {
    throw new Error("Не указывайте учётные данные при чтении расширений.");
  }
  return { kind: connection.kind, value: connection.value.trim() };
}

function designerArguments(connection) {
  const checked = validateConnection(connection);
  return [
    "DESIGNER",
    checked.kind === "file" ? "/F" : checked.kind === "server" ? "/S" : "/IBName",
    checked.value,
    "/DisableStartupMessages",
    "/DisableStartupDialogs",
    "/DumpDBCfgList",
    "-AllExtensions"
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
  // This command's stdout is the canonical list source.  Keep the parser
  // defensive nevertheless: an unexpected diagnostic must not turn a
  // connection string with stored credentials into a selectable name.
  if (hasCredentials(value)) return undefined;
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
  const terminateGraceMs = options.terminateGraceMs ?? TERMINATE_GRACE_MS;
  return new Promise((resolve, reject) => {
    const stdout = [];
    let stdoutLength = 0;
    let child;
    try {
      child = spawn(executable, args, {
        shell: false,
        windowsHide: true,
        stdio: ["ignore", "pipe", "ignore"]
      });
    } catch (error) {
      reject(error);
      return;
    }
    child.stdout?.on("data", (chunk) => {
      // Some releases write DumpDBCfgList directly to stdout instead of the
      // /Out file. Keep a bounded in-memory fallback, never log it.
      if (stdoutLength >= MAX_CAPTURED_OUTPUT_BYTES) return;
      const remaining = MAX_CAPTURED_OUTPUT_BYTES - stdoutLength;
      const accepted = Buffer.from(chunk).subarray(0, remaining);
      stdout.push(accepted);
      stdoutLength += accepted.length;
    });
    let timedOut = false;
    let settled = false;
    let timer;
    let killTimer;
    const timeoutError = () => new Error("Конфигуратор не ответил при чтении списка расширений.");
    const clearTimers = () => {
      clearTimeout(timer);
      clearTimeout(killTimer);
    };
    const settle = (callback, value) => {
      if (settled) return;
      settled = true;
      clearTimers();
      callback(value);
    };
    timer = setTimeout(() => {
      if (settled) return;
      timedOut = true;
      // A Designer process can keep running when a platform dialog appears.
      // Give a normal termination a short grace period before escalating.
      child.kill("SIGTERM");
      killTimer = setTimeout(() => {
        if (settled) return;
        child.kill("SIGKILL");
        // SIGKILL should close a real child shortly, but settling here also
        // prevents the VS Code progress UI from hanging on a broken process.
        settle(reject, timeoutError());
      }, terminateGraceMs);
    }, timeoutMs);
    child.once("error", (error) => {
      settle(reject, error);
    });
    child.once("close", (code) => {
      if (timedOut) {
        settle(reject, timeoutError());
      } else if (code !== 0) {
        settle(reject, new Error("Не удалось прочитать список расширений из информационной базы."));
      } else {
        settle(resolve, Buffer.concat(stdout));
      }
    });
  });
}

/**
 * Reads extension names from an infobase without modifying it.  Designer's
 * /DumpDBCfgList writes the list to standard output; /Out is deliberately not
 * used because it contains service diagnostics instead of the list itself.
 * The caller receives only parsed extension names, never raw command output.
 */
async function discoverInfoBaseExtensions(options) {
  const executable = options.executable ?? await designerExecutable(options.platformDirectory, options);
  const standardOutput = await (options.run ?? run)(
    executable,
    designerArguments(options.connection),
    options
  );
  return parseDesignerExtensionList(standardOutput ?? Buffer.alloc(0));
}

module.exports = {
  DEFAULT_TIMEOUT_MS,
  MAX_CAPTURED_OUTPUT_BYTES,
  TERMINATE_GRACE_MS,
  decodeDesignerOutput,
  designerArguments,
  designerExecutable,
  discoverInfoBaseExtensions,
  executableName,
  parseDesignerExtensionList,
  run,
  validateConnection
};
