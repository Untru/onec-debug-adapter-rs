# onec-debug-adapter-rs

Нативный кроссплатформенный адаптер [Debug Adapter Protocol][dap] для HTTP-сервера отладки 1С:Предприятия. Цель проекта — не требовать от пользователя .NET, Java, Node.js или прав администратора: распространяются готовые бинарники для каждой платформы.

Проект является независимой реализацией протокола. HTTP/XML-интерфейс сервера отладки изучался по открытому проекту [akpaevj/onec-debug-adapter][reference], выпущенному под MIT.

## Статус

Реализована рабочая нативная основа отладки через RDBG:

- DAP `launch`, `attach`, breakpoints, exception breakpoints, stepping, pause, threads и disconnect;
- source/condition/hit-count/logpoint для BSL-модулей основной конфигурации и расширений;
- событие остановки, stack trace, раскрываемые локальные variables и evaluate/hover, включая отложенный RDBG `exprEvaluated`;
- `launch` выбирает `1cv8c` из `platformPath` (указанная версия или `LATEST`) и запускает его с HTTP-отладкой; для файловой базы сам поднимает и затем останавливает `dbgs`, используя выделенный им порт;
- CI проверяет форматирование, Clippy, unit-тесты и упаковку VSIX.

`attach` предназначен для уже доступного сервера отладки 1С. Для `launch` платформа 1С должна быть установлена у пользователя, но Rust, .NET, Node.js и права администратора для адаптера не нужны.

## Локальная сборка

Для разработки нужен Rust 1.85 или новее. У конечного пользователя Rust не нужен:

```sh
cargo build --release
```

Получившийся файл: `target/release/onec-debug-adapter` (`.exe` в Windows). Пример конфигурации находится в [`examples/launch.json`](examples/launch.json).

При публикации тега `v*` GitHub Actions собирает нативные VSIX для Windows x64, Linux x64/ARM64 и macOS x64/Apple Silicon. В каждом VSIX находится только подходящий бинарник адаптера; устанавливать .NET, Rust или Node.js пользователю не нужно.

## Совместимость с VS Code

Расширение регистрирует тот же тип отладчика — `"type": "onec"`, — что и [akpaevj/vsc-onec-debug-adapter](https://github.com/akpaevj/vsc-onec-debug-adapter). Поэтому конфигурации сохраняют `rootProject`, `platformPath`, `platformVersion`, `infoBase`, `debugServerHost`, `debugServerPort`, `extensions` и `autoAttachTypes`; меняется только установленное расширение. Вместо `dotnet` оно запускает подходящий для ОС нативный бинарник из VSIX.

Не устанавливайте оба расширения одновременно: оба владеют типом `onec`. Для разработки до первого release VSIX задайте путь к локальному бинарнику через настройку `onec.nativeAdapterPath`.

Для подключения укажите `infoBaseAlias` — серверный псевдоним информационной базы. Если он совпадает с `infoBase`, достаточно `infoBase`; иначе `infoBaseAlias` имеет приоритет.

[dap]: https://microsoft.github.io/debug-adapter-protocol/
[reference]: https://github.com/akpaevj/onec-debug-adapter
