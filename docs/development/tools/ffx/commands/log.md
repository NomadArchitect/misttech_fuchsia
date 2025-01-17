# Command log

`ffx log` is a log-viewing utility built into `ffx`. This guide describes how to
configure and use `ffx log` to view logs on your Fuchsia device.

To start the log viewer, run the following command:

```posix-terminal
ffx log
```

This command prints the current log contents and leaves the connection open to
stream new log entries. To print the current contents of the log and exit, use `dump`:

```posix-terminal
ffx log dump
```

## Proactive logging

The `ffx` daemon persists in the background after an `ffx` command is run. The daemon proactively
discovers Fuchsia devices and connects to them as they become reachable.

With proactive logging, the `ffx` daemon begins reading logs from a target device in the background
as soon as it's connected. The logs are cached on the host machine, up to a configured space limit.
When the space limit is reached, logs are "rotated": the oldest logs are deleted to make room for
the newest ones.

This means that when you view logs using `ffx log`, logs are actually read from the cache on the
host machine, not directly from the target device. In general, this should not add any noticeable
delay to the log viewer, except in rare cases where the device is producing extremely large log
volumes.

Note: Proactive logging is currently deprecated and is being removed.

### Features

Since logs are cached on the host machine, you can view logs that have been cached from a target
device that are from previous boots of the device. For example, if a device crashes, you might be
able to view the logs from the time just before the crash if they were cached in time.

You can use `ffx log dump` to view logs from a previous session. For example, to view logs from your
device's previous boot:

```posix-terminal
ffx --target <NODENAME> log dump ~1
```

`~1` identifies the session relative to the latest one you want to view, where `0` is reserved for
the currently active session for that target device (whether or not a currently active session exists). You
can view earlier boots by using `~2`, `~3`, and so on.

### Configuration

There are 3 configuration settings relevant to the proactive log cache:

- `proactive_log.max_sessions_per_target`: The maximum number of boot sessions to keep cached on the
  host. Default is 5 (that is, after 6 reboots, the logs from the oldest boot session are deleted).
- `proactive_log.max_session_size_bytes`: The maximum number of bytes to be cached for each session.
  Default is 100MB (that is, after 100MB of logs are on-disk, the oldest chunk of logs for that
  session are deleted)
- `proactive_log.max_log_size_bytes`: The maximum number of bytes to be used in a single log chunk.
  You should not generally need to change this setting. Default is 1MB.

## Symbolization

Logs are symbolized in the background as they are read from the device (before they are written to
the host log cache). However, this background processing means that misconfigurations in the
`symbolizer` host tool or with the symbol index can cause logs to be not symbolized without any
visible warning. Errors encountered when setting up the `symbolizer` tool are logged to the `ffx`
daemon log.

Users working with the Fuchsia source checkout setup do not need to perform any extra configuration;
symbolization takes place automatically as in `fx log`. Users working without the Fuchsia source
checkout setup need to configure the symbol index appropriate to their development environment.

The `ffx log` command tries to detect common misconfigurations in the `symbolizer` tool, but cannot
detect all of them. If your logs are not being symbolized, please
[file a bug](https://bugs.fuchsia.dev/p/fuchsia/issues/entry?template=ffx+User+Bug).

### Configuring symbolizer

There are two configuration parameters relevant to symbolization:

- `proactive_log.symbolize.enabled`: Toggles whether symbolization is attempted. Default is `true`.
- `proactive_log.symbolize.extra_args`: A raw string of additional parameters passed directly to the
  `symbolizer` host tool. This can be used to, for example, configure remote symbol servers. Default
  is `""`.

## Filtering logs

The `ffx log` command provides additional options to filter the logs captured from the target
device. You can apply filters to the log based on timestamps, component, tags, or log level.

```posix-terminal
ffx log --filter hello-world --severity error
```

For a complete list of filtering options, run `ffx log --help`.

## Log settings

Log filters modify how the captured logs are displayed by `ffx log`, but they do not affect the
log entries emitted by components on the target device. Use the `--set-severity` option to send a
request to configure the [log settings][fidl-logsettings] of specific components during the
logging session. This adjusts the log level applied to any component matching the provided
[component selector][component-select] for recording logs.

```posix-terminal
ffx log --set-severity {{ '<var>' }}component-selector{{ '</var>' }}#{{ '<var>' }}log-level{{ '</var>' }}
```

You can use this to temporarily enable logs that are below the minimum severity configure by your
component, such as `DEBUG` or `TRACE` logs, or to suppress noisy logs from a component to improve
performance.

The following example enables debug logs for the `core/audio` component, and suppresses all log
messages except errors from networking components:

```none {:.devsite-disable-click-to-copy}
$ ffx log --set-severity core/audio#DEBUG --set-severity core/network/**#ERROR
```

Note: Unlike the `--severity` option, which filters the view after logs are captured from the
target device, this configures whether the target components emit logs of the given severity.

[component-select]: /docs/development/tools/ffx/commands/component-select.md
[fidl-logsettings]: https://fuchsia.dev/reference/fidl/fuchsia.diagnostics#LogSettings
