const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const {
  discoverPlatformDirectories,
  hasCredentials,
  isSamePath,
  noExtensionSourceChoices,
  parseExtensionName,
  uniqueConfigurationName,
  validatePlatformDirectory
} = require("../setup-wizard");

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
