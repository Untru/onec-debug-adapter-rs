const assert = require("node:assert/strict");
const { EventEmitter } = require("node:events");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { PassThrough } = require("node:stream");
const test = require("node:test");
const {
  configurationSummary,
  commonInfoBaseListPaths,
  decodePlatformText,
  defaultIbaseDirectories,
  defaultIbaseFiles,
  discoverPlatformDirectories,
  discoverIbaseEntries,
  discoverV8ProjectEntries,
  hasCredentials,
  ibasesFilePickerChoice,
  isSamePath,
  mergeInfoBaseEntries,
  noExtensionSourceChoices,
  parseExtensionName,
  parseIbaseV8i,
  parseV8Project,
  uniqueConfigurationName,
  validatePlatformDirectory
} = require("../setup-wizard");
const {
  designerArguments,
  designerExecutable,
  discoverInfoBaseExtensions,
  parseDesignerExtensionList,
  run,
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

test("summarizes the standalone server launch mode", () => {
  const summary = configurationSummary({
    name: "1C: Demo (launch)",
    request: "launch",
    launchMode: "standaloneServer",
    rootProject: "/work/src",
    infoBase: "/work/ib",
    platformPath: "/opt/1cv8/8.3.27",
    platformVersion: "LATEST",
    debugServerHost: "localhost",
    debugServerPort: 1550,
    standaloneServerHost: "localhost",
    standaloneServerPort: 8314,
    standaloneServerBase: "/",
    standaloneServerTransport: "direct",
    standaloneServerName: "onec-debug-1941",
    standaloneServerDirectRegPort: 1941,
    standaloneServerDirectRange: "1960:1991",
    standaloneServerDataPath: "/work/.vscode/onec-standalone-server",
    autoAttachTypes: ["ManagedClient", "Server"]
  });
  assert.match(summary, /автономный сервер \(ibsrv\)/);
  assert.match(summary, /прямой TCP\/IP \(onec-debug-1941\)/);
  assert.match(summary, /localhost:8314\//);
  assert.doesNotMatch(summary, /SSH/);
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

[Сохранённые учётные данные]
Connect=Srvr="srv-1c";Ref="Secret";Usr="admin";Pwd="secret";
`);
  assert.deepEqual(entries, [
    {
      name: "Файловая база",
      kind: "file",
      filePath: "/Users/me/1c/demo",
      server: undefined,
      reference: undefined,
      hasStoredCredentials: false
    },
    {
      name: "Серверная база",
      kind: "server",
      filePath: undefined,
      server: "srv-1c",
      reference: "Accounting",
      hasStoredCredentials: false
    },
    {
      name: "Сохранённые учётные данные",
      kind: "server",
      filePath: undefined,
      server: "srv-1c",
      reference: "Secret",
      hasStoredCredentials: true
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
    assert.deepEqual(entries.map((entry) => [entry.name, entry.filePath, entry.hasStoredCredentials]), [
      ["Safe", "/tmp/safe", false],
      ["Secure", undefined, true],
      ["Shared", "/tmp/first", false],
      ["Shared", "/tmp/second", false]
    ]);
    const secure = entries.find((entry) => entry.name === "Secure");
    assert.deepEqual(secure, {
      name: "Secure",
      kind: "server",
      filePath: undefined,
      server: "host",
      reference: "db",
      hasStoredCredentials: true,
      sourceFile: second
    });
    assert.equal(JSON.stringify(entries).includes("Pwd"), false);
    assert.equal(JSON.stringify(entries).includes("no\""), false);
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
});

test("reads personal and CommonInfoBases lists from the 1CEStart directory", async () => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "onec-common-ibases-"));
  const startup = path.join(temporary, ".1C", "1cestart");
  const shared = path.join(temporary, "shared", "company.v8i");
  await fs.mkdir(path.dirname(shared), { recursive: true });
  await fs.mkdir(startup, { recursive: true });
  await fs.writeFile(path.join(startup, "ibases.v8i"), [
    "[Personal]",
    "Connect=File=\"/tmp/personal\";",
    "[Duplicate]",
    "Connect=Srvr=\"srv\";Ref=\"same\";"
  ].join("\n"));
  await fs.writeFile(shared, [
    "[Company]",
    "Connect=Srvr=\"srv\";Ref=\"company\";",
    "[Duplicate]",
    "Connect=Srvr=\"srv\";Ref=\"same\";"
  ].join("\n"));
  const config = "CommonInfoBases=../../shared/company.v8i;\n";
  await fs.writeFile(
    path.join(startup, "1cestart.cfg"),
    Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(config, "utf16le")])
  );
  try {
    const entries = await discoverIbaseEntries({
      startupDirectories: [startup],
      files: [path.join(startup, "ibases.v8i")]
    });
    assert.deepEqual(entries.map((entry) => [entry.name, entry.sourceFile]), [
      ["Company", shared],
      ["Duplicate", path.join(startup, "ibases.v8i")],
      ["Personal", path.join(startup, "ibases.v8i")]
    ]);
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
});

test("parses CommonInfoBases and platform encodings", () => {
  assert.deepEqual(commonInfoBaseListPaths("Other=ignored\nCommonInfoBases=one.v8i; two.v8i \n"), ["one.v8i", "two.v8i"]);
  assert.equal(
    decodePlatformText(Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from("CommonInfoBases=x", "utf16le")])),
    "CommonInfoBases=x"
  );
  assert.equal(decodePlatformText(Buffer.from([0xef, 0xbb, 0xbf, 0x5b, 0x44, 0x5d])), "[D]");
});

test("imports an explicitly selected ibases.v8i outside standard launcher locations", async () => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "onec-picked-ibases-"));
  const selectedFile = path.join(temporary, "portable", "custom-list.v8i");
  await fs.mkdir(path.dirname(selectedFile), { recursive: true });
  await fs.writeFile(
    selectedFile,
    "[Portable file base]\nConnect=File=\"/tmp/portable-ib\";\n"
      + "[Saved credentials stay private]\nConnect=Srvr=\"srv\";Ref=\"private\";Usr=\"user\";Pwd=\"secret\";"
  );
  try {
    const entries = await discoverIbaseEntries({ files: [selectedFile] });
    assert.deepEqual(entries.map((entry) => ({
      name: entry.name,
      kind: entry.kind,
      sourceFile: entry.sourceFile,
      hasStoredCredentials: entry.hasStoredCredentials
    })), [
      {
        name: "Portable file base",
        kind: "file",
        sourceFile: selectedFile,
        hasStoredCredentials: false
      },
      {
        name: "Saved credentials stay private",
        kind: "server",
        sourceFile: selectedFile,
        hasStoredCredentials: true
      }
    ]);
    assert.equal(JSON.stringify(entries).includes("secret"), false);
    assert.equal(JSON.stringify(entries).includes("user"), false);
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
});

test("offers an explicit file picker for an ibases.v8i outside default locations", () => {
  assert.deepEqual(ibasesFilePickerChoice(), {
    label: "$(file) Выбрать файл ibases.v8i…",
    description: "Импортировать список баз из другого расположения",
    ibasesFile: true
  });
});

test("uses native launcher locations for each operating system", () => {
  assert.deepEqual(
    defaultIbaseFiles("darwin", { HOME: "/Users/test" }),
    [
      path.join("/Users/test", ".1C", "1cestart", "ibases.v8i"),
      path.join("/Users/test", "Library", "Application Support", "1C", "1CEStart", "ibases.v8i"),
      path.join("/Users/test", ".1cv8", "1C", "1CEStart", "ibases.v8i")
    ]
  );
  assert.deepEqual(
    defaultIbaseFiles("linux", { HOME: "/home/test" }),
    [
      path.join("/home/test", ".1C", "1cestart", "ibases.v8i"),
      path.join("/home/test", ".1cv8", "1C", "1CEStart", "ibases.v8i")
    ]
  );
  assert.deepEqual(
    defaultIbaseFiles("win32", { APPDATA: "C:\\Users\\test\\AppData\\Roaming" }),
    [path.join("C:\\Users\\test\\AppData\\Roaming", "1C", "1CEStart", "ibases.v8i")]
  );
});

test("uses the current 1CEStart startup directory on macOS and Linux", () => {
  assert.equal(defaultIbaseDirectories("darwin", { HOME: "/Users/test" })[0], "/Users/test/.1C/1cestart");
  assert.equal(defaultIbaseDirectories("linux", { HOME: "/home/test" })[0], "/home/test/.1C/1cestart");
});

test("reads only safe v8-project infobase identity fields and resolves file paths", () => {
  const projectFile = "/work/demo/.v8-project.json";
  const entries = parseV8Project(projectFile, JSON.stringify({
    default: "autotest",
    databases: [
      {
        id: "autotest",
        name: "Автотесты",
        type: "file",
        path: "build/ib",
        password: "must not be read",
        aliases: ["ignored"]
      },
      {
        id: "server",
        name: "Серверная",
        type: "server",
        server: "srv-1c",
        ref: "Accounting",
        usr: "ignored"
      },
      {
        id: "bad",
        type: "file",
        path: "File=/tmp/ib;Pwd=secret;"
      }
    ]
  }));
  assert.deepEqual(entries, [
    {
      id: "autotest",
      name: "Автотесты",
      kind: "file",
      filePath: "/work/demo/build/ib",
      isDefault: true,
      source: "v8-project",
      sourceFile: projectFile
    },
    {
      id: "server",
      name: "Серверная",
      kind: "server",
      server: "srv-1c",
      reference: "Accounting",
      isDefault: false,
      source: "v8-project",
      sourceFile: projectFile
    }
  ]);
  assert.equal(JSON.stringify(entries).includes("password"), false);
  assert.equal(JSON.stringify(entries).includes("usr"), false);
});

test("discovers a workspace v8-project file and prefers its default while deduplicating launcher bases", async () => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "onec-v8-project-"));
  const projectFile = path.join(temporary, ".v8-project.json");
  await fs.writeFile(projectFile, JSON.stringify({
    default: "demo",
    databases: [{ id: "demo", name: "Из проекта", type: "file", path: "ib" }]
  }));
  try {
    const projectEntries = await discoverV8ProjectEntries(temporary);
    const merged = mergeInfoBaseEntries(projectEntries, [{
      name: "В лаунчере",
      kind: "file",
      filePath: path.join(temporary, "ib"),
      sourceFile: "/tmp/ibases.v8i"
    }]);
    assert.equal(merged.length, 1);
    assert.equal(merged[0].name, "Из проекта");
    assert.equal(merged[0].isDefault, true);
    assert.equal(merged[0].source, "v8-project");
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
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

test("builds a read-only Designer command that writes the list to stdout", () => {
  assert.deepEqual(
    designerArguments({ kind: "file", value: "/tmp/demo-ib" }),
    [
      "DESIGNER",
      "/F",
      "/tmp/demo-ib",
      "/DisableStartupMessages",
      "/DisableStartupDialogs",
      "/DumpDBCfgList",
      "-AllExtensions"
    ]
  );
});

test("builds a read-only Designer command for a server infobase", () => {
  const args = designerArguments(
    { kind: "server", value: "srv-1c\\accounting" }
  );
  assert.deepEqual(args.slice(0, 4), ["DESIGNER", "/S", "srv-1c\\accounting", "/DisableStartupMessages"]);
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

test("does not turn unexpected credential output into an extension name", () => {
  assert.deepEqual(
    parseDesignerExtensionList("Pwd=secret\nExtension: Safe_Extension\n"),
    ["Safe_Extension"]
  );
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

test("reads the Designer extension inventory only from canonical stdout", async () => {
  let receivedArguments;
  const names = await discoverInfoBaseExtensions({
    executable: "/mock/1cv8",
    connection: { kind: "registered", value: "Demo" },
    run: async (_executable, args) => {
      receivedArguments = args;
      return Buffer.from("Extension: Ext_One\nExt_Two\n");
    }
  });
  assert.deepEqual(names, ["Ext_One", "Ext_Two"]);
  assert.equal(receivedArguments.includes("/Out"), false);
});

test("escalates a timed-out Designer process to SIGKILL and settles", async () => {
  const child = new EventEmitter();
  child.stdout = new PassThrough();
  const signals = [];
  child.kill = (signal) => {
    signals.push(signal);
    return true;
  };
  await assert.rejects(
    run("/mock/1cv8", [], {
      spawn: () => child,
      timeoutMs: 5,
      terminateGraceMs: 5
    }),
    /не ответил/
  );
  assert.deepEqual(signals, ["SIGTERM", "SIGKILL"]);
});

test("clears the SIGKILL escalation when Designer closes after SIGTERM", async () => {
  const child = new EventEmitter();
  child.stdout = new PassThrough();
  const signals = [];
  child.kill = (signal) => {
    signals.push(signal);
    if (signal === "SIGTERM") {
      setTimeout(() => child.emit("close", null), 1);
    }
    return true;
  };
  await assert.rejects(
    run("/mock/1cv8", [], {
      spawn: () => child,
      timeoutMs: 5,
      terminateGraceMs: 25
    }),
    /не ответил/
  );
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.deepEqual(signals, ["SIGTERM"]);
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
