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
- Images, `/diff`, `/review`, `/files`, and `@` file completion say `Not available on remote servers yet`. Pasted paths are blocked only when the local image detector recognizes them as client-side images, resolving relative paths from the client working directory. Other text, including server paths and error messages, stays in the input box. Git branch and change counts are hidden until server workspace queries are implemented.
- A configured `statusLine` script runs on the client with `/` as its working directory, outside the client project. Its JSON input still describes the server's `cwd`, with `gitBranch` set to null. Use absolute paths for scripts and resources.
- `serve`, `upgrade`, `rollback`, `service`, `mcp`, `skills install`, token administration, and every `auth` subcommand reject `--server` with `Run this on the server machine`. Remote `--session-file` is also rejected.
- A protocol mismatch names the server and asks you to upgrade it on that machine. Remote clients never replace or start a server locally. A failed remote `doctor` connection does not fall back to local offline diagnostics.
- After approximately 45 seconds without stream data (three 15-second heartbeats), the connection times out. The terminal and remote `exec` reconnect using the last event sequence. `/v1/info.instance` distinguishes reconnecting to the same process from a restarted server. A client does not automatically resend a submitted message. Remote `exec` prints each distinct reconnect failure to stderr; unauthorized (HTTP 401) or incompatible-protocol failures end the command with an error instead of retrying indefinitely.

These instructions are generic and do not depend on a cloud vendor. The development checks use temporary loopback servers and a fake model; an actual relay, certificate, and network trial must be checked in the deployment environment.
