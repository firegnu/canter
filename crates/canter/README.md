# canter CLI

Run `canter --help` for interactive options and subcommands.

## Publish the current session

`canter --session-file <path>` writes a JSON file for programs following this
terminal's session. It also works with `--resume` and `-c` / `--continue`.
Relative paths are resolved from the terminal's starting directory; the parent
directory must already exist. Give each terminal its own path.

```json
{"sessionId":"s-example","cwd":"/absolute/project","stateDir":"/absolute/state"}
```

`cwd` is the session's working directory, including when resuming a session from
another directory. `stateDir` is the terminal's absolute `CANTER_HOME`, or
`$HOME/.canter` when unset. Fields use camelCase; future additions are compatible,
so readers should ignore unknown fields. No environment variables or credentials
are included.

The file is first published when a session is created or resumed, after any
folder trust or session selection prompt. It is rewritten on `/new`, `/resume`,
`/fork`, handoff and return, and after reconnecting to the server (even when the
session ID is unchanged). Viewing a sub-agent's screen keeps the main session's
identity. Before a session exists, no file is published. Each write uses a
temporary file in the same directory and an atomic rename, with mode 0600.
Exit leaves the last file in place; it is not a liveness indicator. Watch the
parent directory or reopen the path to see replacements.

Resuming a child directly or returning to a child after handoff publishes its
parent session ID and the parent's working directory, matching the UI's main
session. Reconnecting while viewing a child also keeps that main identity.

An unwritable destination at startup reports an error and exits with nonzero
status before taking over the terminal. A failed initial publication also exits;
later write failures show an English error in the UI and leave it running. The
next session switch or reconnect retries publication. Without the option, the
terminal does not write a session file. `canter exec` does not accept this option.

To find the server, read `<stateDir>/server.json`. Its `socketPath` identifies the
Unix socket for HTTP + SSE; `protocolVersion`, `version` and `instance` identify
the protocol, server version and process instance. Probe `GET /v1/info` through
that socket and check its `protocolVersion` and `serverVersion` against the
registration. A registration file alone does not prove the server is running.
After a disconnect, reread the registration and reconnect.

Observers should subscribe using `GET /v1/events?session=<sessionId>&observe=true`
so they do not keep the terminal's client lifecycle open. See the
[protocol documentation](../canter-protocol/README.md) for events and replay.
