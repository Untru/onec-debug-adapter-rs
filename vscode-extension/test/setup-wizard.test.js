const assert = require("node:assert/strict");
const test = require("node:test");
const {
  hasCredentials,
  isSamePath,
  parseExtensionName,
  uniqueConfigurationName
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
