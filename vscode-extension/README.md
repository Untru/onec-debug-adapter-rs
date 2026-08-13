# 1C:Enterprise Native Debugger

This VS Code extension registers the compatible `onec` debugger type and starts a native `onec-debug-adapter` executable over standard input/output.

It preserves the `launch.json` fields used by `akpaevj/vsc-onec-debug-adapter`, so existing configurations can be reused. Do not install both extensions at the same time because both own `type: "onec"`.

Release VSIX files bundle the native adapter for their target OS/architecture. For development builds, set `onec.nativeAdapterPath` to a local debug or release build of `onec-debug-adapter`.

## Installing a release VSIX

1. Download the matching `onec-debug-native-<target>.vsix` from [GitHub Releases](https://github.com/Untru/onec-debug-adapter-rs/releases).
2. In VS Code, open **Extensions** → `…` → **Install from VSIX…**, or run:

   ```sh
   code --install-extension onec-debug-native-<target>.vsix
   ```

3. Run **1C: Настроить отладку…** from the Command Palette. The wizard validates the main configuration, lets you select any number of extension source directories, and writes a separate configuration to the selected workspace folder’s `.vscode/launch.json`. Existing launch configurations are preserved.

   In a multi-root workspace, first choose the folder that should receive `launch.json`. Extension candidates are found only within that workspace folder; use **Добавить каталоги вне рабочей области…** if an extension’s sources live elsewhere. The wizard keeps no credentials and does not enable tracing.

   `rootProject` is the source root of the base configuration and must contain `Configuration.xml`. Every selected extension source root must also contain `Configuration.xml`; its `Properties/Name` must match the name of the extension installed in the infobase. Duplicate paths and duplicate extension names are rejected before the configuration is written.

4. Alternatively, add a minimal `.vscode/launch.json` yourself:

   ```json
   {
     "version": "0.2.0",
     "configurations": [
       {
         "name": "1C: Launch",
         "type": "onec",
         "request": "launch",
         "rootProject": "/absolute/path/to/unpacked-configuration",
         "platformPath": "/absolute/path/to/1c-platform-versions",
         "platformVersion": "LATEST",
         "infoBase": "/absolute/path/to/file-infobase",
         "extensions": [
           "/absolute/path/to/unpacked-configuration-extension"
         ]
       }
     ]
   }
   ```

Do not install this extension and the original `vsc-onec-debug-adapter` together: both register `type: "onec"`.

The adapter supports attach and launch, source/conditional/hit-count/log breakpoints, runtime-error breakpoints, stepping, stack frames, local variables and expression evaluation. `launch` starts the platform's `1cv8c` client from `platformPath`; `attach` expects an already available 1C debug server. Both requests require `rootProject` for reliable BSL source mapping, breakpoints and source navigation; `attach` does not need `platformPath` because it does not start the platform. For an unregistered file infobase, set `infoBase` to its absolute directory path or `File="/path/to/ib";`; launch uses `/F` and does not modify the user's 1C launcher list.

On macOS, select the server-platform directory that contains both `1cv8c` and `dbgs`, normally `/opt/1cv8/<version>` (for example `/opt/1cv8/8.3.27.1508`). A `1cv8.app` bundle and its `Contents/MacOS` directory only provide the GUI application and cannot be used for `launch`.
