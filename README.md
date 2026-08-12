# Зачем? 
Женя большой молодец, что написал адаптер и это очень круто, но там где он мне был нужен нельзя было уcтановить NET 6


# onec-debug-adapter-rs

Нативный кроссплатформенный адаптер [Debug Adapter Protocol][dap] для HTTP-сервера отладки 1С:Предприятия. Цель проекта — не требовать от пользователя .NET, Java, Node.js или прав администратора: распространяются готовые бинарники для каждой платформы.

Проект является независимой реализацией протокола. HTTP/XML-интерфейс сервера отладки изучался по открытому проекту [akpaevj/onec-debug-adapter][reference], выпущенному под MIT.

## Статус

Реализована рабочая нативная основа отладки через RDBG:

- DAP `launch`, `attach`, breakpoints, exception breakpoints, stepping, pause, threads и disconnect;
- source/condition/hit-count/logpoint для BSL-модулей основной конфигурации и расширений;
- событие остановки, stack trace, раскрываемые локальные variables и evaluate/hover, включая отложенный RDBG `exprEvaluated`;
- панель «Цели отладки» в VS Code: список доступных 1С-сеансов, ручное подключение и обновление списка при старте/завершении цели;
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

## Установка VSIX

1. На странице [GitHub Releases](https://github.com/Untru/onec-debug-adapter-rs/releases) скачайте файл `onec-debug-native-<target>.vsix` для своей ОС и архитектуры. Текущий опубликованный prerelease — `v0.1.0-alpha.23`.
2. В VS Code откройте **Extensions**, нажмите `…` и выберите **Install from VSIX…**. Либо выполните:

   ```sh
   code --install-extension onec-debug-native-<target>.vsix
   ```

3. Создайте `.vscode/launch.json`, например:

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

Не устанавливайте одновременно это расширение и оригинальный `vsc-onec-debug-adapter`: оба регистрируют `type: "onec"`.

## Совместимость с VS Code

Расширение регистрирует тот же тип отладчика — `"type": "onec"`, — что и [akpaevj/vsc-onec-debug-adapter](https://github.com/akpaevj/vsc-onec-debug-adapter). Поэтому конфигурации сохраняют `rootProject`, `platformPath`, `platformVersion`, `infoBase`, `debugServerHost`, `debugServerPort`, `extensions` и `autoAttachTypes`; меняется только установленное расширение. Вместо `dotnet` оно запускает подходящий для ОС нативный бинарник из VSIX.

Не устанавливайте оба расширения одновременно: оба владеют типом `onec`. Для разработки до первого release VSIX задайте путь к локальному бинарнику через настройку `onec.nativeAdapterPath`.

Для подключения к серверной базе укажите `infoBaseAlias` — серверный псевдоним информационной базы. Если он совпадает с `infoBase`, достаточно `infoBase`; иначе `infoBaseAlias` имеет приоритет.

Файловую базу не нужно добавлять в общий список 1С: для `launch` в `infoBase` можно передать абсолютный путь к каталогу базы или строку `File="/путь/к/базе";`. Адаптер передаст его клиенту через `/F`, поднимет временный `dbgs` и использует обязательный для файловой базы RDBG-alias `DefAlias` автоматически. Это не требует прав администратора и не меняет `ibases.v8i` пользователя.

## Замер задержек отладчика

Чтобы исследовать задержки F10/F11 и загрузки переменных, добавьте в нужную конфигурацию запуска `"trace": true`. По умолчанию адаптер запишет JSONL в `.vscode/onec-debug-latency.jsonl` под `rootProject`; для отдельного пути укажите `traceFile`. Когда `trace` не задан или равен `false`, файл и каталог не создаются.

```json
"trace": true,
"traceFile": "/tmp/onec-debug-latency.jsonl"
```

Каждая строка содержит `schemaVersion`, монотонный `tsMs`, `event` и поля корреляции (`traceId`, `threadId`, `targetId` или `pollId`). В трассе есть DAP-запрос/ответ шага, вызов RDBG, жизненный цикл long-poll, остановка с call stack, а также evaluate/variables и отложенный `exprEvaluated`.

[dap]: https://microsoft.github.io/debug-adapter-protocol/
[reference]: https://github.com/akpaevj/onec-debug-adapter
