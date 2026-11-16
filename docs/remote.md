# Remote connections

The server runs on the machine that holds your code. Each server serves one person. A relay forwards HTTP and SSE; it does not run the agent or hold a checkout. The terminal and `canter exec` use the same protocol over a Unix socket locally, or TCP remotely.

Without `--server`, canter always finds or starts the local server as before. There is no default remote. Server and client each have their own `CANTER_HOME` (default `~/.canter`). Configure models and credentials on the server machine. This initial remote setup is for API-key providers; ChatGPT account administration stays on the server machine.

## Prepare the server

Merge this into the existing server `config.json`, keeping its model configuration:

```json
{
  "remote": {
    "listen": ["127.0.0.1:8790"]
  }
}
```

Choose an unused port. Only loopback IP addresses are accepted; omitted or empty `remote.listen` leaves TCP off. Start `canter serve` in a terminal on the server machine. If an older daemon is running, explicitly replace it on that machine with `canter serve --replace`. Listener changes take effect at startup. `canter doctor` there reports the actual listeners and token status.

Issue a separate token for each client, on the server machine:

```sh
canter remote token add terminal
canter remote token list
```

The first command prints the token once. Transfer it privately to the client. It grants access to execute work as the server user. The server keeps only its hash in `remote-tokens.json` (0600); the client stores the token in `remotes.json` (0600). Do not put the token in a URL, command argument, repository, or log.

To revoke it later, run `canter remote token remove terminal` on the server machine. New requests are rejected and existing streams close. Removing a client-side remote entry does not revoke its server token.

## Try SSH forwarding directly

If the client can SSH to the server machine, run this on the client in a separate terminal:

```sh
ssh -N -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
  -L 127.0.0.1:8791:127.0.0.1:8790 server.example.com
```

Use an SSH host entry or supply your SSH username for that host. Leave this command running in the foreground. It forwards client port 8791 through SSH to the server's loopback port 8790. This does not assume a shared LAN: direct SSH must already be reachable. If it is not, use the relay below.

In another client terminal:

```sh
canter remote add work http://127.0.0.1:8791
```

At the `Paste the token, then press Enter` prompt, paste the token and press Enter. Terminal input is hidden and reads one line; Ctrl+D is not needed. Alternatively, redirect a private token file or pipe into this command as before. The token is not an argument. HTTP is allowed here because the destination is a literal loopback address and SSH encrypts the network hop. Other remote HTTP addresses are refused; HTTPS uses normal certificate verification. Redirects and environment HTTP proxies are not followed.

```sh
canter remote list
canter --server work --cwd /srv/project
canter exec --server work --cwd /srv/project 'Explain the project'
canter --server work --resume
canter exec --server work --last 'Continue'
canter usage --server work
canter stats --server work
canter doctor --server work
```

`--cwd` is an absolute path **on the server**, required when creating a session. The server rejects relative paths. When resuming with `-c`, `exec --last`, or the session picker, an explicit `--cwd` filters the list; without it, all remote directories are eligible. An explicit session ID uses that session's directory. No local path is guessed. To remove the saved connection, run `canter remote remove work`.

## Reach a private server through a VPS

The server machine and client both need outbound SSH access to the relay; they do not need to reach each other. On the server machine, keep a reverse tunnel running:

```sh
ssh -N -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
  -R 127.0.0.1:8792:127.0.0.1:8790 relay.example.com
```

The relay's SSH daemon must allow TCP forwarding. Keep the relay listener on loopback, with `GatewayPorts no`; do not expose port 8792 publicly. OpenSSH documents the directions and binding rules for [`-L` and `-R`](https://man.openbsd.org/ssh.1).

For a trial without a domain or HTTPS proxy, run this on the client:

```sh
ssh -N -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
  -L 127.0.0.1:8791:127.0.0.1:8792 relay.example.com
```

Then add `http://127.0.0.1:8791` as above. Both network hops use SSH. These two tunnels are sufficient for a terminal trial across unrelated networks.

## Keep the tunnels and the server running

Tunnels started by hand stop when the network drops, the proxy changes, the laptop sleeps or a machine restarts. Let the operating system restart them instead. canter itself needs no change: the terminal and remote `exec` reconnect on their own once the path is back.

The SSH keys used here must work without a passphrase prompt or a running agent (`BatchMode=yes`), because these services run without a terminal.

**Relay.** Have the SSH daemon drop dead connections quickly, so a reconnecting server is not refused because a stale `-R` forward still holds the relay port. Add `/etc/ssh/sshd_config.d/canter-relay.conf` (the directory is included by default on Debian and Ubuntu):

```
ClientAliveInterval 15
ClientAliveCountMax 3
```

Check with `sudo sshd -t`, then `sudo systemctl reload ssh`; a reload keeps existing connections. A dead connection is then dropped after about 45 seconds. To undo, remove the file and reload.

**Linux server machine.** Allow user services to run without a login session, once (it takes effect immediately, no restart needed):

```sh
sudo loginctl enable-linger "$USER"
```

`~/.config/systemd/user/canter-relay-tunnel.service`:

```ini
[Unit]
Description=canter relay tunnel
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=0

[Service]
ExecStart=/usr/bin/ssh -N -o BatchMode=yes -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o ConnectTimeout=15 -R 127.0.0.1:8792:127.0.0.1:8790 relay.example.com
Restart=always
RestartSec=10

[Install]
WantedBy=default.target
```

Linux has no `canter service install`, and a remote client never starts a server. To have the server start at boot and come back after a crash, add `~/.config/systemd/user/canter-server.service`:

```ini
[Unit]
Description=canter server
StartLimitIntervalSec=60
StartLimitBurst=3

[Service]
Environment=CANTER_HOME=%h/.canter
WorkingDirectory=%h/.canter
ExecStart=%h/.cargo/bin/canter serve
StandardOutput=append:%h/.canter/server.log
StandardError=append:%h/.canter/server.log
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

Adjust `ExecStart` to where `canter` is installed. Stop a server that is already running first (`canter serve` refuses to start beside one), then:

```sh
systemctl --user daemon-reload
systemctl --user enable --now canter-server.service canter-relay-tunnel.service
```

`Restart=on-failure` restarts the server after a crash, not after a normal exit. The start limit stops retrying after three failed starts in a minute, for example when another canter server already holds the state directory. `canter serve --replace` and `canter upgrade` still work: the old server exits normally and systemd leaves it stopped. The new server, however, is not managed by systemd, so it is not restarted after a crash until the next boot (or until you stop it and run `systemctl --user start canter-server.service`). Logs: `journalctl --user -u canter-relay-tunnel.service` and `~/.canter/server.log`.

**macOS client.** `~/Library/LaunchAgents/local.canter-tunnel.work.plist`, where `work` is the remote name:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>local.canter-tunnel.work</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/bin/ssh</string><string>-N</string>
    <string>-o</string><string>BatchMode=yes</string>
    <string>-o</string><string>ExitOnForwardFailure=yes</string>
    <string>-o</string><string>ServerAliveInterval=15</string>
    <string>-o</string><string>ServerAliveCountMax=3</string>
    <string>-o</string><string>ConnectTimeout=15</string>
    <string>-L</string><string>127.0.0.1:8791:127.0.0.1:8792</string>
    <string>relay.example.com</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>StandardErrorPath</key><string>/Users/you/Library/Logs/canter-tunnel-work.log</string>
</dict>
</plist>
```

launchd does not expand `~` or `$HOME`; write the full path. Stop any tunnel already holding port 8791, then load it now (it also starts at every login):

```sh
plutil -lint ~/Library/LaunchAgents/local.canter-tunnel.work.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/local.canter-tunnel.work.plist
launchctl print gui/$(id -u)/local.canter-tunnel.work
```

To remove it: `launchctl bootout gui/$(id -u)/local.canter-tunnel.work`, then delete the file.

To check the whole path from the client, `curl --noproxy '*' http://127.0.0.1:8791/v1/info` should answer HTTP 401 (no token sent).

## Add HTTPS with Caddy

For direct HTTPS clients, keep the server-to-relay reverse tunnel, and run Caddy on the relay. Point a domain such as `canter.example.com` at the relay, allow inbound ports 80/443, and use this minimal Caddyfile:

```caddyfile
canter.example.com {
    reverse_proxy 127.0.0.1:8792
}
```

Caddy manages the public certificate and renewal when its [automatic HTTPS requirements](https://caddyserver.com/docs/automatic-https) are met. Its [reverse proxy flushes `text/event-stream` responses immediately](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy#streaming); do not add buffering or strip the Authorization header. The relay terminates HTTPS and can see the plaintext and token. Secure and administer it as part of your server environment.

On the client, create a separate entry using the HTTPS address:

```sh
canter remote add work-https https://canter.example.com
canter --server work-https --cwd /srv/project
```

Again, the token comes from stdin. No client-to-relay SSH tunnel is needed with this entry. The examples use SSH as the tunnel transport; an already-administered frp tunnel can instead expose the same relay loopback port for Caddy to reach.

## What differs remotely

- Client environment variables are omitted for new, resumed and forked sessions. The server supplies its own session environment; commands, hooks and tools run there.
- File links copy the server path. Web links open as usual (an SSH terminal still follows its existing copy-only behavior).
- `/memory` lists memories and supports deletion on the server. Selecting an entry to edit says `Edit memory on the server machine`. `/export` writes on the client: relative paths use the client working directory and `~` uses the client HOME.
- Clipboard images and dropped image files are read on the client and sent as image data, just as for local connections. Relative image paths use the client working directory. Each image may be at most 8 MiB, with 64 MiB of original image bytes per input and no image count limit; the client checks before sending. Other pasted text, including server paths and error messages, stays in the input box. Older servers without `imageData` must be upgraded on the server machine.
- `/diff`, `/review`, `/files`, `@` file completion, git branch and changed-line counts use the server's session workspace. PR review uses `gh` and its login on the server machine.
- A configured `statusLine` script runs on the client with `/` as its working directory, outside the client project. Its JSON input describes the server's `cwd`, `gitBranch` and `gitDiff`. Use absolute paths for scripts and resources.
- `serve`, `upgrade`, `rollback`, `service`, `mcp`, `skills install`, token administration, and every `auth` subcommand reject `--server` with `Run this on the server machine`. Remote `--session-file` is also rejected.
- A protocol mismatch names the server and asks you to upgrade it on that machine. Remote clients never replace or start a server locally. A failed remote `doctor` connection does not fall back to local offline diagnostics.
- After approximately 45 seconds without stream data (three 15-second heartbeats), the connection times out. The terminal and remote `exec` reconnect using the last event sequence. `/v1/info.instance` distinguishes reconnecting to the same process from a restarted server. A client does not automatically resend a submitted message. Remote `exec` prints each distinct reconnect failure to stderr; unauthorized (HTTP 401) or incompatible-protocol failures end the command with an error instead of retrying indefinitely.

These instructions are generic and do not depend on a cloud vendor. The development checks use temporary loopback servers and a fake model; an actual relay, certificate, and network trial must be checked in the deployment environment.
