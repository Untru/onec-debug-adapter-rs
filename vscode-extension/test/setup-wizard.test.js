const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const {
  defaultIbaseFiles,
  discoverPlatformDirectories,
  discoverIbaseEntries,
  hasCredentials,
  isSamePath,
  noExtensionSourceChoices,
  parseExtensionName,
  parseIbaseV8i,
  uniqueConfigurationName,
  validatePlatformDirectory
} = require("../setup-wizard");
const {
  designerArguments,
  designerExecutable,
  discoverInfoBaseExtensions,
  parseDesignerExtensionList,
  validateConnection
} = require("../extension-inventory");

test("reads an extension name from Properties", () => {
  assert.equal(
    parseExtensionName("<MetaDataObject><Properties><Name>\u0422\u0435\u0441\u0442&amp;\u0418\u043c\u044f</Name></Properties></MetaDataObject>"),
    "\u0422\u0435\u0441\u0442&\u0418\u043c\u044f"
  );
});

test("does not accept a name outside Properties when Properties exists", () => {
  assert.equal(
    parseExtensionName("<Name>\u041b\u043e\u0436\u043d\u043e\u0435</Name><Properties><Name>\u041d\u0430\u0441\u0442\u043e\u044f\u0449\u0435\u0435</Name></Properties>"),
    "\u041d\u0430\u0441\u0442\u043e\u044f\u0449\u0435\u0435"
  );
});

test("makes a generated debug configuration name unique", () => {
  const configurations = [{ name: "1C: Demo (запуск)" }, { name: "1C: Demo (запуск) (2)" }];
  assert.equal(uniqueConfigurationName(configurations, "1C: Demo (запуск)"), "1C: Demo (запуск) (3)");
});

test("recognizes credential-bearing connection strings", () => {
  assert.equal(hasCredentials('File="/tmp/ib";Pwd="secret";'), true);
  assert.equal(hasCredentials("DemoBase"), false);
});

test("parses safe file and server entries from ibases.v8i", () => {
  const entries = parseIbaseV8i(`
[Файловая база]
Connect=File="/Users/me/1c/demo";

[Серверная база]
Connect=Srvr="srv-1c";Ref="Accounting";

[Не переносить пароль]
Connect=Srvr="srv-1c";Ref="Secret";Usr="admin";Pwd="secret";
`);
  assert.deepEqual(entries, [
    {
      name: "Файловая база",
      kind: "file",
      filePath: "/Users/me/1c/demo",
      server: undefined,
      reference: undefined
    },
    {
      name: "Серверная база",
      kind: "server",
      filePath: undefined,
      server: "srv-1c",
      reference: "Accounting"
    }
  ]);
});

test("discovers ibases from standard files and keeps no credentials", async () => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "onec-ibases-"));
  const first = path.join(temporary, "first.v8i");
  const second = path.join(temporary, "second.v8i");
  await fs.writeFile(first, "[Shared]\nConnect=File=\"/tmp/first\";\n[Safe]\nConnect=File=\"/tmp/safe\";");
  await fs.writeFile(second, "[Shared]\nConnect=File=\"/tmp/second\";\n[Secure]\nConnect=Srvr=\"host\";Ref=\"db\";Pwd=\"no\";");
  try {
    const entries = await discoverIbaseEntries({ files: [first, second] });
    assert.deepEqual(entries.map((entry) => [entry.name, entry.filePath]), [
      ["Safe", "/tmp/safe"],
      ["Shared", "/tmp/first"]
    ]);
    assert.equal(entries.some((entry) => entry.name === "Secure"), false);
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
});

test("uses native launcher locations for each operating system", () => {
  assert.deepEqual(
    defaultIbaseFiles("darwin", { HOME: "/Users/test" }),
    [
      "/Users/test/Library/Application Support/1C/1CEStart/ibases.v8i",
      "/Users/test/.1cv8/1C/1CEStart/ibases.v8i"
    ]
  );
  assert.deepEqual(
    defaultIbaseFiles("linux", { HOME: "/home/test" }),
    ["/home/test/.1cv8/1C/1CEStart/ibases.v8i"]
  );
  assert.deepEqual(
    defaultIbaseFiles("win32", { APPDATA: "C:\\Users\\test\\AppData\\Roaming" }),
    ["C:\\Users\\test\\AppData\\Roaming/1C/1CEStart/ibases.v8i"]
  );
});

test("compares normalized paths", () => {
  assert.equal(isSamePath("./example", "example"), true);
});

test("offers an explicit continuation when no extension sources are found", () => {
  const choices = noExtensionSourceChoices();
  assert.equal(choices[0].label, "Продолжить без расширений");
  assert.equal(choices[0].continueWithoutExtensions, true);
  assert.equal(choices[1].browse, true);
});

test("discovers a runnable macOS platform under /opt/1cv8", async () => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "onec-platform-"));
  const platformRoot = path.join(temporary, "opt", "1cv8");
  const versionRoot = path.join(platformRoot, "8.3.27.1508");
  await fs.mkdir(versionRoot, { recursive: true });
  await Promise.all(["1cv8c", "dbgs"].map((name) => fs.writeFile(path.join(versionRoot, name), "")));

  try {
    const discovered = await discoverPlatformDirectories({ platform: "darwin", roots: [platformRoot] });
    assert.deepEqual(discovered, [await fs.realpath(versionRoot)]);
    assert.equal(
      await validatePlatformDirectory(platformRoot, { platform: "darwin" }),
      await fs.realpath(platformRoot)
    );
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
});

test("rejects a macOS GUI application bundle as a launch platform", async () => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "onec-platform-app-"));
  const bundle = path.join(temporary, "1cv8.app", "Contents", "MacOS");
  await fs.mkdir(bundle, { recursive: true });
  await fs.writeFile(path.join(bundle, "1cv8"), "");

  try {
    await assert.rejects(
      validatePlatformDirectory(bundle, { platform: "darwin" }),
      /\/opt\/1cv8\/<версия>/
    );
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
});

test("builds a read-only Designer command for a file infobase", () => {
  assert.deepEqual(
    designerArguments({ kind: "file", value: "/tmp/demo-ib" }, "/tmp/result.txt"),
    [
      "DESIGNER",
      "/F",
      "/tmp/demo-ib",
      "/DisableStartupMessages",
      "/DumpDBCfgList",
      "-AllExtensions",
      "/Out",
      "/tmp/result.txt"
    ]
  );
});

test("reads localized and plain extension names from Designer result", () => {
  const output = [
    "Список расширений конфигурации:",
    "Расширение: Моя_Проверка",
    "- Sales Extension",
    "Моя_Проверка",
    "Информация"
  ].join("\n");
  assert.deepEqual(parseDesignerExtensionList(output), ["Моя_Проверка", "Sales Extension"]);
});

test("reads a UTF-16LE Designer result", () => {
  const utf16 = Buffer.concat([
    Buffer.from([0xff, 0xfe]),
    Buffer.from("Ext_One\r\nExt_Two\r\n", "utf16le")
  ]);
  assert.deepEqual(parseDesignerExtensionList(utf16), ["Ext_One", "Ext_Two"]);
});

test("does not allow credentials in the Designer inventory connection", () => {
  assert.throws(
    () => validateConnection({ kind: "registered", value: 'Srv="demo";Pwd="secret";' }),
    /учётные данные/
  );
});

test("finds the Designer executable in a Windows version directory", async () => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "onec-designer-"));
  const bin = path.join(temporary, "bin");
  await fs.mkdir(bin);
  await fs.writeFile(path.join(bin, "1cv8.exe"), "");
  try {
    assert.equal(
      await designerExecutable(temporary, { platform: "win32" }),
      path.join(bin, "1cv8.exe")
    );
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
});

test("cleans private Designer output after reading extension inventory", async () => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "onec-inventory-test-"));
  try {
    const names = await discoverInfoBaseExtensions({
      executable: "/mock/1cv8",
      tempRoot: temporary,
      connection: { kind: "registered", value: "Demo" },
      run: async (_executable, args) => {
        await fs.writeFile(args[args.length - 1], "Extension: Ext_One\nExt_Two\n");
      }
    });
    assert.deepEqual(names, ["Ext_One", "Ext_Two"]);
    assert.deepEqual(await fs.readdir(temporary), []);
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
});

test("discovers a runnable Windows platform under Program Files style root", async () => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "onec-platform-win-"));
  const root = path.join(temporary, "1cv8");
  const binaryDirectory = path.join(root, "8.3.28.1000", "bin");
  await fs.mkdir(binaryDirectory, { recursive: true });
  await Promise.all(["1cv8c.exe", "dbgs.exe"].map((name) => fs.writeFile(path.join(binaryDirectory, name), "")));
  try {
    assert.deepEqual(
      await discoverPlatformDirectories({ platform: "win32", roots: [root] }),
      [path.join(await fs.realpath(root), "8.3.28.1000", "bin")]
    );
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
});
