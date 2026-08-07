# 1C:Enterprise Native Debugger

This VS Code extension registers the compatible `onec` debugger type and starts a native `onec-debug-adapter` executable over standard input/output.

It preserves the `launch.json` fields used by `akpaevj/vsc-onec-debug-adapter`, so existing configurations can be reused. Do not install both extensions at the same time because both own `type: "onec"`.

Release VSIX files bundle the native adapter for their target OS/architecture. For development builds, set `onec.nativeAdapterPath` to a local debug or release build of `onec-debug-adapter`.

The adapter supports attach and launch, source/conditional/hit-count/log breakpoints, runtime-error breakpoints, stepping, stack frames, local variables and expression evaluation. `launch` starts the platform's `1cv8c` client from `platformPath`; `attach` expects an already available 1C debug server.
