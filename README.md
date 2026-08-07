# onec-debug-adapter-rs

Нативный кроссплатформенный адаптер [Debug Adapter Protocol][dap] для HTTP-сервера отладки 1С:Предприятия. Цель проекта — не требовать от пользователя .NET, Java, Node.js или прав администратора: распространяются готовые бинарники для каждой платформы.

Проект является независимой реализацией протокола. HTTP/XML-интерфейс сервера отладки изучался по открытому проекту [akpaevj/onec-debug-adapter][reference], выпущенному под MIT.

## Статус

Первая итерация готова:

- строгое чтение и запись DAP-сообщений по `stdin`/`stdout`;
- `initialize`, `launch`/`attach`, `configurationDone`, `threads`, `disconnect` и `terminate`;
- проверка доступности HTTP-сервера 1С и регистрация/отключение Debug UI;
- CI с форматированием, Clippy и тестами.

Текущая итерация реализует основу XML-сессии RDBG. Следующая часть: начальные настройки, список предметов отладки и точки останова. До её завершения адаптер корректно сообщает, что остальные команды пока не реализованы.

## Локальная сборка

Для разработки нужен Rust 1.85 или новее. У конечного пользователя Rust не нужен:

```sh
cargo build --release
```

Получившийся файл: `target/release/onec-debug-adapter` (`.exe` в Windows). Пример конфигурации находится в [`examples/launch.json`](examples/launch.json).

## Совместимость с VS Code

Готовящееся расширение будет регистрировать тот же тип отладчика — `"type": "onec"`, — что и [akpaevj/vsc-onec-debug-adapter](https://github.com/akpaevj/vsc-onec-debug-adapter). Поэтому существующие `launch.json` сохранят `rootProject`, `platformPath`, `platformVersion`, `infoBase`, `debugServerHost`, `debugServerPort`, `extensions` и `autoAttachTypes`; менять нужно будет только установленное расширение. Вместо `dotnet` оно запустит подходящий для ОС нативный бинарник из VSIX.

`attach` работает с сервером отладки на другой машине. Для режима `launch` в будущих версиях локально должна быть установлена платформа 1С соответствующей ОС.

Для подключения укажите `infoBaseAlias` — серверный псевдоним информационной базы. Если он совпадает с `infoBase`, достаточно `infoBase`; иначе `infoBaseAlias` имеет приоритет.

[dap]: https://microsoft.github.io/debug-adapter-protocol/
[reference]: https://github.com/akpaevj/onec-debug-adapter
