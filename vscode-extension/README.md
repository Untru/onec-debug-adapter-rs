# 1C:Enterprise Native Debugger

This VS Code extension registers the compatible `onec` debugger type and starts a native `onec-debug-adapter` executable over standard input/output.

It preserves the `launch.json` fields used by `akpaevj/vsc-onec-debug-adapter`, so existing configurations can be reused. Do not install both extensions at the same time because both own `type: "onec"`.

Until release binaries are bundled, set `onec.nativeAdapterPath` to a local debug or release build of `onec-debug-adapter`.
