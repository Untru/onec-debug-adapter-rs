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

3. Run **1C: Настроить отладку…** from the Command Palette. The click-through wizard adds a separate configuration to the selected workspace folder’s `.vscode/launch.json`; existing launch configurations are preserved.

   The wizard proceeds in this order:

   1. **Platform.** It discovers runnable installed versions and lists only directories that contain both `1cv8c` and `dbgs`: `%ProgramFiles%\\1cv8\\<version>\\bin` on Windows, `/opt/1cv8/<version>` on macOS, and the usual `/opt/1cv8` and `/opt/1C/v8.3/...` roots on Linux. A different directory can be picked and is validated before continuing. On macOS, do not choose `1cv8.app`; launch needs the server-platform directory under `/opt/1cv8`.
   2. **Infobase.** It reads launcher registrations from `ibases.v8i`, so a user can select a registered base by name. It searches `%APPDATA%` and `%LOCALAPPDATA%` on Windows, `~/Library/Application Support/1C/1CEStart` and `~/.1cv8/1C/1CEStart` on macOS, and `~/.1cv8/1C/1CEStart` on Linux. An unregistered file-infobase directory or a registered name can also be entered manually.
   3. **Sources and extensions.** Select the base-configuration source root containing `Configuration.xml`. The wizard invokes the selected platform’s Designer in batch mode with `DESIGNER /DumpDBCfgList -AllExtensions` for the selected base. This is a read-only inventory: its private temporary result is removed afterwards. For every extension enabled in the infobase, the wizard asks for the matching source root, preferring candidates whose `Properties/Name` agrees with the installed extension. Users can browse for a source root or skip an extension; a mismatched name is rejected. If Designer cannot obtain the list, the wizard falls back to manual extension-source selection.
   4. **Debug mode and review.** Choose launch or attach, set the debug server address, and confirm the generated configuration.

   In a multi-root workspace, first choose the folder that should receive `launch.json`. Source candidates are discovered only within that workspace folder, but a source directory outside it can be selected manually. The wizard never writes credentials, user names, credentialed connection strings or tracing settings. For a registered infobase it saves only the registration name, leaving launcher credentials inside `ibases.v8i` and out of the UI.

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
