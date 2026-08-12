# 1C:Enterprise Native Debugger

This VS Code extension registers the compatible `onec` debugger type and starts a native `onec-debug-adapter` executable over standard input/output.

It preserves the `launch.json` fields used by `akpaevj/vsc-onec-debug-adapter`, so existing configurations can be reused. Do not install both extensions at the same time because both own `type: "onec"`.

Release VSIX files bundle the native adapter for their target OS/architecture. For development builds, set `onec.nativeAdapterPath` to a local debug or release build of `onec-debug-adapter`.

## Installing a release VSIX

1. Download the matching `onec-debug-native-<target>.vsix` from [GitHub Releases](https://github.com/Untru/onec-debug-adapter-rs/releases). The current published prerelease is `v0.1.0-alpha.23`.
2. In VS Code, open **Extensions** → `…` → **Install from VSIX…**, or run:

   ```sh
   code --install-extension onec-debug-native-<target>.vsix
   ```

3. Add a minimal `.vscode/launch.json`:

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
         "infoBase": "/absolute/path/to/file-infobase"
       }
     ]
   }
   ```

Do not install this extension and the original `vsc-onec-debug-adapter` together: both register `type: "onec"`.

The adapter supports attach and launch, source/conditional/hit-count/log breakpoints, runtime-error breakpoints, stepping, stack frames, local variables and expression evaluation. `launch` starts the platform's `1cv8c` client from `platformPath`; `attach` expects an already available 1C debug server. For an unregistered file infobase, set `infoBase` to its absolute directory path or `File="/path/to/ib";`; launch uses `/F` and does not modify the user's 1C launcher list.
