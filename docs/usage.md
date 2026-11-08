# canter usage

canter is a coding agent with one resident server per machine and many clients. This guide covers the configuration file, the command line, the terminal interface, permissions, tools, and the optional integrations. For why canter is built this way, read [`DESIGN.md`](DESIGN.md).

canter is developed and used on macOS. The sandbox and the login service use macOS features.

## Getting started

Build from a clone with Rust stable:

```sh
git clone https://github.com/firegnu/canter.git
cd canter
cargo install --locked --path crates/canter
```

Then create `~/.canter/config.json` (mode 0600) with at least one model provider. A minimal DeepSeek setup:

```json
{
  "model": "deepseek/deepseek-flash",
  "providers": {
    "deepseek": {
      "base_url": "https://api.deepseek.com",
      "api_key": "YOUR_DEEPSEEK_API_KEY",
      "models": {
        "deepseek-flash": {
          "context_window": 1048576,
          "image": true
        }
      }
    }
  }
}
```

Run `canter` in a project directory:

```sh
cd ~/code/my-project
canter
```

The first client starts the server in the background and opens a full-screen terminal session. The server keeps running after the terminal exits, so you can resume the session from another terminal.

Before a new or resumed session opens in a folder canter has not seen, it asks `Trust this folder?` with two choices: `Trust and continue`, which records the repository root in `trusted_folders.json`, or `Open restricted`, which opens this one session with LSP and hooks disabled. Cancel creates no session. Trust is recorded per Git repository root; worktrees of one repository share it. Non-Git directories are trusted individually. `canter exec` never asks: it opens an untrusted folder restricted and says so on stderr.

To use a ChatGPT plan, add a `responses` provider to `config.json` (see the provider examples below), then sign in:

```sh
canter auth login chatgpt
```

Set `CANTER_HOME` to use another absolute state directory; all files below then live there instead of `~/.canter`.

## State directory

`CANTER_HOME`, or `~/.canter` when unset. The directory is 0700 and the state files canter creates there are 0600. canter keeps its state here and never reads another agent's state directory.

| Path | Contents |
| --- | --- |
| `config.json` | Model providers, provider keys, prices, hooks, MCP servers and global switches. |
| `last_model.json`, `last_model.lock` | Last model and thinking effort chosen with `/model` or `/think`, used when a new session leaves the model empty; stores `model`, `reasoning_effort` and `config_model`, plus the lock that serializes writes. |
| `auth/chatgpt/` | ChatGPT sign-in records: host ID, accounts, tokens and per-account consent (directory 0700, files 0600). |
| `rules.json` | Permanent command and file rules written by approvals or `/permissions`. |
| `trusted_folders.json` | Canonical repository roots the user trusted. |
| `tui.json` | Terminal settings saved by `/statusline` and `/theme`: status line items, theme, notifications. The server does not read it. |
| `AGENTS.md` | Global instructions, loaded at session open. |
| `input_history.jsonl` | The terminal's own input history, one JSON line per sent input, at most 2000 entries. The server does not read it. |
| `input_history.lock` | Lock used by terminals writing the input history. |
| `agents/` | Custom sub-agent roles, one Markdown file per role. |
| `themes/` | Optional `.tmTheme` files listed by `/theme`. |
| `memory/` | Global memories, one Markdown file each. |
| `projects/<key>/memory/` | Project memories; all worktrees of a repository share one key. |
| `sessions/<id>/` | Events and the private model history for one session, plus image copies. |
| `trash/<id>/` | Sessions moved to the trash by the session list. |
| `usage-archive.jsonl` | Daily usage summaries of sessions removed by automatic cleanup. |
| `mcp-auth/` | MCP OAuth tokens, one file per service, bound to its URL. |
| `models/chatgpt/` | Cached ChatGPT model catalog, one file per account; contains no tokens. |
| `backups/` | Programs saved by `canter upgrade` and `canter rollback`; the newest five are kept. |
| `server.sock` | The Unix socket the server listens on; canter does not open a TCP port. |
| `server.lock`, `server.json` | The single-instance lock and the registration (PID, version, socket path, instance). |
| `server.log` | Server stdout and stderr when a client or an upgrade starts it. |
| `startup.lock`, `replace.lock`, `replace.json`, `install.lock` | Locks and handover state for automatic starts and upgrades. |
| `config.lock` | Lock used by `canter mcp ...` while rewriting `config.json`. |

`rules.json` is read afresh for every permission check; edit it while canter runs. The other files are read at the stated times.

### Instructions and AGENTS.md

canter reads instructions when a session opens:

- Global: `<CANTER_HOME>/AGENTS.md`.
- Project: in each directory from the Git repository root down to the session's working directory, canter reads `AGENTS.md`; a directory without `AGENTS.md` but with `CLAUDE.md` works too, reading its `CLAUDE.md` instead. A directory that has both is read as `AGENTS.md` only. Claude Code's `@path` imports inside `CLAUDE.md` are kept as plain text.

Each file is limited to 64 KiB. Edits apply to new sessions; a session already open keeps the instructions it started with. Restricted sessions still load global and project instructions and skills.

## Configuration

`config.json` is JSON. canter re-validates it after every save and applies most changes at the next turn boundary without restarting the server. The file must remain readable by the server; canter resets its permissions to 0600 when it starts.

There are two ways to describe providers. The legacy form configures one Chat Completions endpoint with top-level fields. The `providers` form configures one or more providers, each with its own credentials and models, and can be mixed with the legacy form. New configurations should use `providers`.

### Top-level fields

| Field | Default | Meaning |
| --- | --- | --- |
| `model` | required | The fallback default model. A new session without an explicit model uses the last `/model` or `/think` selection when it is still available; otherwise this model. Changing this value clears the remembered selection. In a providers-only configuration, write `<provider>/<model>`; the first `/` separates them and the model name itself may contain `/`. |
| `providers` | `{}` | Named providers. See below. |
| `models` | legacy only | Models offered by the legacy endpoint; defaults to `["deepseek-flash", "deepseek-v4-pro"]`. The default model is added at the front if missing. Ignored when the configuration has only `providers`. |
| `image_models` | legacy only | Models that accept images; defaults to `["deepseek-flash"]`. An empty list disables image input. Ignored when the configuration has only `providers`. |
| `base_url` | legacy only | Chat Completions base URL for legacy model names. HTTPS is required unless the host is localhost or a loopback address. |
| `api_key` | legacy only | Key for the legacy endpoint. |
| `context_window` | legacy only | Context window for every legacy model. Must be greater than 81920. |
| `auto_compact_tokens` | window minus 65536 minus 16384 | Compact automatically when the context reaches this many tokens. Must be positive. If larger than the default line, the default line wins. |
| `headers` | `{}` | Extra headers for legacy Chat Completions requests. See [Request headers](#request-headers). |
| `prices` | built-in table | Per-model price overrides in USD per million tokens. See [Prices](#prices). |
| `mcp_servers` | `{}` | MCP servers. See [MCP](#mcp). |
| `lsp` | disabled | Language servers. See [Language servers](#language-servers). |
| `hooks` | `{}` | Hooks. See [Hooks](#hooks). |
| `prompt_suggestions` | `true` | Ask the model for a one-line next input after a turn, and show an opening example from recent Git commits. Suggestions are never saved. |
| `plan_enabled` | `true` | Offer the plan checklist tools. Reloads at the next turn boundary. |
| `mcp_tool_search` | `true` | Offer `mcp_search` and `mcp_call` instead of every MCP tool definition. Reloads at the next turn boundary. |
| `telemetry` | `true` | Write local `telemetry.*` events to the session event file. Nothing is sent over the network. |
| `sandbox` | `true` | Run model shell commands in the macOS Seatbelt sandbox. See [Sandbox](#sandbox). |
| `session_retention_days` | `30` | Delete sessions (and trash) unused for more than this many days. `0` never deletes. |
| `auto_wake_limit` | `10` | Maximum consecutive background wakes of an idle session per user message. `0` disables automatic wakes. Reloads at the next turn boundary. |
| `goal_budget_usd` | `5.0` | Default dollar budget for a new `/goal`. `0` means unlimited. |
| `model_max_retries` | `10` | Retries after the first model request, `0` to `100`. |
| `statusLine` | none | Terminal-only: a shell command that draws the status rows. See [Status line](#status-line). |

### Provider objects

Each key under `providers` is a provider name made of ASCII letters, digits, `-`, `_` or `.`.

| Field | Default | Meaning |
| --- | --- | --- |
| `api` | `"chat_completions"` | Wire protocol: `chat_completions` or `responses`. |
| `auth` | none | Only `"chatgpt"` is accepted, and only with `responses`. |
| `base_url` | required for `chat_completions` | Endpoint base URL. HTTPS unless localhost or loopback. |
| `api_key` | required for `chat_completions` | Provider key. |
| `headers` | `{}` | Extra Chat Completions headers, same rules as the top-level field. |
| `models` | required, non-empty | Map of model name to model settings. |

A `responses` provider must use `"auth": "chatgpt"` and must not set `base_url` or `api_key`; it always uses the official Responses endpoint and the signed-in account. Every provider must list at least one model. After ChatGPT sign-in, the account's model catalog adds models and fills in details for models that are not written by hand; a hand-written entry overrides the catalog. The configured `model` must be available from the configuration: one of the written provider models, or a legacy model when legacy fields are present.

Each model object accepts:

| Field | Default | Meaning |
| --- | --- | --- |
| `context_window` | required | The real context window, greater than 81920. |
| `image` | `false` | Whether the model accepts image input. |
| `auto_compact_tokens` | window minus 65536 minus 16384 | Per-model automatic compaction line, positive. |
| `price` | unknown | USD per million tokens, in the same shape as `prices`. Unknown cost is shown as `unknown` or `+?`, never guessed. |
| `supported_reasoning_efforts` | `["none", "low", "high", "max"]` | Thinking levels the model accepts. |
| `default_reasoning_effort` | `"high"` | Level used when this model is chosen for a new session without an explicit or remembered effort. Must be in the supported list. |

Thinking level names are `none`, `low`, `medium`, `high`, `xhigh` and `max`. An empty list, an unknown name, or a default outside the list makes the whole configuration invalid.

Several providers can run at once. Each session has one current model; providers can be switched while the session is idle. `GET /v1/models` shows every configured model with its provider and window.

Example with an OpenAI-compatible provider and the official DeepSeek provider:

```json
{
  "model": "go/deepseek-v4.1-flash",
  "providers": {
    "go": {
      "base_url": "https://opencode.ai/inference/go/openai/v1",
      "api_key": "YOUR_OPENCODE_KEY",
      "headers": { "x-opencode-session": "{session_id}" },
      "models": {
        "deepseek-v4.1-flash": { "context_window": 1048576 }
      }
    },
    "deepseek": {
      "base_url": "https://api.deepseek.com",
      "api_key": "YOUR_DEEPSEEK_API_KEY",
      "models": {
        "deepseek-flash": {
          "context_window": 1048576,
          "auto_compact_tokens": 400000,
          "image": true
        }
      }
    }
  }
}
```

A ChatGPT provider:

```json
{
  "model": "chatgpt/YOUR_MODEL",
  "providers": {
    "chatgpt": {
      "api": "responses",
      "auth": "chatgpt",
      "models": {
        "YOUR_MODEL": {
          "context_window": 200000,
          "image": true,
          "supported_reasoning_efforts": ["low", "medium", "high", "xhigh", "max"],
          "default_reasoning_effort": "medium"
        }
      }
    }
  }
}
```

Replace `YOUR_MODEL` and the window with the model's actual values. OpenAI's Responses API is used with `store: false`, streaming, full input on every request, and the selected reasoning effort. A ChatGPT-plan request does not add to the dollar budget and has no dollar price; token counts are still recorded.

The legacy single-provider form still works, alone or beside `providers`. Its model names have no provider prefix:

```json
{
  "base_url": "https://api.deepseek.com",
  "model": "deepseek-flash",
  "models": ["deepseek-flash", "deepseek-v4-pro"],
  "image_models": ["deepseek-flash"],
  "context_window": 1048576,
  "api_key": "YOUR_DEEPSEEK_API_KEY"
}
```

### Request headers

`headers` is an object of string names to string values, used only for Chat Completions model requests, including retries. It is not sent to web fetch, web search or MCP.

- `{session_id}` in a value is replaced with the current session's ID. The ID is stable across compaction, reopening and server restarts; a sub-agent uses its own session ID. If a request has no session ID, a header containing the placeholder is omitted.
- Other text is sent unchanged.
- Names and values must be valid HTTP header values. `Authorization`, `Content-Type` and `User-Agent` cannot be overridden, case-insensitively. Every model request already carries `User-Agent: canter/<version>`.
- Header values are treated like the API key: they do not appear in logs, events, errors or telemetry.

### Prices

Amounts are USD per million tokens. The built-in table covers `deepseek-flash` and `deepseek-v4-pro` at DeepSeek's published prices. Peak hours are Monday through Friday, UTC `[01:00, 04:00)` and `[06:00, 10:00)`; everything else, including weekends, is off-peak. Chinese public holidays are not modelled, so weekday holidays are priced as peak.

Override or add a price under `prices`, keyed by the full model name:

```json
"prices": {
  "go/deepseek-v4.1-flash": {
    "peak": { "cache_hit": 0.10, "cache_miss": 1.00, "output": 2.00 },
    "off_peak": { "cache_hit": 0.05, "cache_miss": 0.50, "output": 1.00 }
  }
}
```

`peak` requires all three amounts. `off_peak` is optional; when omitted, the peak amounts apply all day. A model with no price records an unknown cost. Changing prices does not rewrite amounts already recorded.

### Reloading

The server watches `config.json`, merges rapid saves, and validates the new file with the same rules as at startup. A valid change is picked up at the next turn boundary: model, credentials, headers, windows, compaction lines, prices, sandbox, telemetry, hooks, MCP and LSP settings, and the tool-list switches `plan_enabled` and `mcp_tool_search`. A turn already running keeps the old configuration. A bad file is rejected whole and the last valid configuration stays active, with a warning event. The socket and listening address do not change.

Changing `model` in `config.json` also clears the selection remembered from `/model` and `/think`, so the new configured model wins; changing any other setting does not clear it. At startup canter compares the remembered selection's `config_model` with the current configured model and clears it if they differ.

`tui.json` is not hot-reloaded: a terminal reads it when it starts. `statusLine` in `config.json` is also read by the terminal at startup.

## Command line

```text
canter [options] [prompt]
canter [options] --resume [session id]
canter [options] -c
canter serve
canter serve --replace
canter service install | uninstall
canter upgrade [directory] [--now]
canter rollback [--now]
canter usage
canter doctor
canter skills install [--force]
canter auth <subcommand>
canter exec [options] [prompt]
canter stats [options]
canter mcp <subcommand>
```

### Starting a session

| Option | Meaning |
| --- | --- |
| `prompt` | Text after the options is sent as the first message of a new session. Not allowed with `--resume` or `--continue`. |
| `-m`, `--model <model>` | Use this model for the session instead of the remembered or configured default. `-m` does not change the remembered selection. |
| `--add-dir <path>` | Add a directory to the session. Repeatable. |
| `--tag <key=value>` | Label a new session. Repeatable; the last value for a key wins. Not allowed when resuming. |
| `-r`, `--resume [id]` | Resume a session. Without an id, open the session picker. |
| `-c`, `--continue` | Resume the most recent session in the current directory. |
| `--session-file <path>` | Write the current session's JSON identity to this path. Relative paths are resolved from the current directory. |
| `--close-on-exit` | Close every session this terminal opened when it exits. |
| `--yolo` | Use the `allow-all` permission mode and disable the sandbox for this open, regardless of `config.json`. |
| `-V`, `--version` | Print the version and commit. |
| `-h`, `--help` | Print the command summary. |

The session picker shows this directory's sessions, most recently updated first. `/` starts a content search, Tab switches between this directory and all directories, `d` moves the selected session to the trash, Enter opens it, and Esc closes the picker. Search matches all words in order-independent fashion against user and assistant messages; it does not search tool output or the trash.

`--session-file` writes:

```json
{"sessionId":"s-...","cwd":"/absolute/project","stateDir":"/absolute/state"}
```

It is written when a session is created or resumed, and rewritten when the terminal switches session, forks, revisits a handoff source, or reconnects after a server upgrade. It is not written by non-interactive runs.

### Server and service

| Command | Meaning |
| --- | --- |
| `canter serve` | Run the server in the foreground. |
| `canter serve --replace` | Start a replacement server in the background and exit when it is listening. The old server waits for active work, up to 60 seconds. |
| `canter service install` | macOS only: write `~/Library/LaunchAgents/com.canter.server.plist` so one server starts at the next login. It starts once and does not restart after exit or a crash. The file saves the installing terminal's `PATH`, `HOME` and `CANTER_HOME` for future server starts; reinstall after changing `PATH`. It does not start or stop the current server. |
| `canter service uninstall` | Remove that plist. The running server and all canter data stay. |

### Upgrade and rollback

`canter upgrade [directory]` builds the current code from the source directory recorded at install time, or from the given canter checkout. It builds into a temporary location, checks the result, backs up the installed program to `<CANTER_HOME>/backups/`, atomically installs the new one, then replaces the server. A bad source directory or failed build leaves the installed program and server untouched.

By default it waits for running turns, goals and background work to finish before replacing the server. Ctrl-C cancels the wait; the program is already installed, and `canter serve --replace` can switch the server later. `--now` skips the wait and uses the usual 60-second drain instead.

`canter rollback [--now]` restores the newest backup, first backing up the current program so you can switch back. It keeps the newest five backups and only manages backups it created. `--now` behaves as above.

### Usage, statistics and doctor

| Command | Meaning |
| --- | --- |
| `canter usage` | Daily token usage and cost by model, across sessions, in the server's local time zone. Uses the server, starting it if needed. |
| `canter stats` | List recent sessions with number, last activity, directory and first message. |
| `canter stats --all` | List every non-sub-agent session, not only recent ones. |
| `canter stats <number\|id>` | Show one session's report. Numbers are positions in the current list. |
| `canter stats --last` | Show the most recently updated session's report. |
| `--json` | Print the list or report as JSON. |
| `--html` | Print one session's report as HTML; needs a session number, ID or `--last`. |
| `canter doctor` | Check configuration, sign-in, providers, MCP and sandbox. It prints `ok`, `fail` or `skip` per check, with a reason and fix under failures, and exits non-zero when any check fails. It never starts, replaces or repairs the server and supports no `--fix`. When no compatible server is reachable, it runs local checks and marks server-dependent checks `skip`. |
| `canter skills install [--force]` | Copy the bundled `docx`, `pptx`, `web-testing` and `xlsx` skills to `~/.agents/skills/`. Existing skill directories are skipped unless `--force` is given, which overwrites only `SKILL.md` and keeps other files. Applies to new sessions. Browser scripts cannot run while the sandbox is enabled: use `canter --yolo` for a session that needs headless scripts, or use Playwright MCP instead. |

### Authentication

```text
canter auth login chatgpt
canter auth status [chatgpt]
canter auth use chatgpt <email|number>
canter auth background chatgpt on|off
canter auth logout chatgpt [email|number]
```

`login` prints a privacy notice, asks whether tasks may keep using the ChatGPT plan after the terminal closes, and opens the official sign-in page in the browser. The consent is stored per account and can be changed with `background`. `status` lists the local accounts and the current one. `use` changes the current account. `logout` clears local access and ID tokens and revokes the refresh token on a best-effort basis; if OpenAI does not confirm, the local token is deleted anyway and the command says so. These commands use the server and start it if none is running.

Credentials live only in `<CANTER_HOME>/auth/chatgpt/` (directory 0700, files 0600), are never printed, and never enter the repository. The login URL, code and tokens are not exposed to other clients or session events.

### MCP commands

```sh
canter mcp add <name> [--env K=V]... -- <command> [args...]
canter mcp add <name> --url <url> [--header K=V]...
canter mcp list
canter mcp remove <name>
canter mcp login <name>
canter mcp logout <name>
```

`list` prints only each name and transport. `add` rejects duplicate names and mixed stdio/HTTP arguments. `login` and `logout` manage OAuth credentials for an HTTP server and do not need a running server. These commands edit canter's own `config.json`; they never read another agent's configuration. See [MCP](#mcp).

## Terminal interface

canter takes over the whole terminal: the conversation is above, the input box is fixed at the bottom. Startup shows the current directory, model, permission mode and a few keys. A session opens on the most recent state; while a turn runs you can keep reading and scrolling.

### Input and keys

| Key | Action |
| --- | --- |
| Enter | Send the input, or run the selected command. In a list, complete first. |
| Shift+Enter, Ctrl+J | New line. |
| `/` | Start a slash command. |
| `/<skill>` | Use a skill by name. |
| `!command` | Run a shell command directly while the session is idle. A single trailing `&` runs it in the background; `&&` is not treated as background. |
| `@` | Mention a file; Tab completes the selected file. |
| Tab | Complete the selected command or file. While a turn is running, Tab steers the input into the running turn instead of queueing it. |
| Esc | Close the `/` or `@` list; otherwise interrupt the turn. Before the model has produced anything, Esc takes the message back. |
| Ctrl+V | Attach the image on the clipboard (macOS). |
| Ctrl+G | Edit the draft in `$VISUAL` or `$EDITOR`. |
| Ctrl+R | Search this terminal's previous input, newest first. ↑/↓ or Ctrl+R selects, Enter replaces the input without sending, Esc restores the draft. |
| Ctrl+B | Move the running foreground shell command to the background. |
| Ctrl+O | Show or fold thinking, long command output and long diffs. |
| Ctrl+F | Find in the conversation. PgUp/PgDn scroll a page; the mouse wheel scrolls three lines; End jumps to the bottom when the input is empty. |
| ↑ / ↓ | Select in a list; otherwise move between input lines. With an empty input, cycle earlier and later messages sent in this directory. |
| Tab / → | Accept a grey prompt suggestion. |
| Shift+Tab | Next mode: Always ask → Ask for execution → Allow all → Plan → Always ask. |
| Drag | Select text; releasing copies it. Double-click copies a word, triple-click a row. Click opens a link or a path in a card. |
| Ctrl+A, Ctrl+E, Home, End | Start or end of the input line. |
| Ctrl+K, Ctrl+U | Delete to the end or the start of the line. |
| Alt+← / Ctrl+← / Alt+B | Back a word. |
| Alt+→ / Ctrl+→ / Alt+F | Forward a word. |
| Alt+Backspace / Ctrl+W | Delete the word before the cursor. |
| Alt+Delete / Alt+D | Delete the word after the cursor. |
| Ctrl+C | Clear the input; quit when it is empty. |
| Ctrl+D | Quit when the input is empty. |
| `?` | Show the shortcuts and commands when the input is empty. |
| Pasting more than 10 lines or 1000 characters | Folds into a `[Pasted #N]` placeholder. |

### Slash commands

Commands are typed at the start of the input. A word after `/` is a command only when it has no other `/`; `/tmp/file` is ordinary text.

| Command | Arguments | What it does |
| --- | --- | --- |
| `/add-dir` | `<path>` | Add a directory to the session. |
| `/agents` | | List this session's sub-agents; open one or stop a running one. |
| `/btw` | `<question>` | Ask one question with the current context without adding it to history. The answer appears in gray in this terminal only and does not enter the saved conversation. |
| `/clear` | | Start a new session; same as `/new`. |
| `/compact` | | Summarize the conversation to free context. |
| `/context` | | Show the loaded context by category, token estimates, the window, the compaction line and the last model-reported usage. Read-only. |
| `/copy` | `[N]` | Copy the last answer, or its code block N, to the clipboard. |
| `/diff` | | Show uncommitted Git changes. |
| `/doctor` | | Run the same checks as `canter doctor`, in a popup. |
| `/exit` | | Quit canter. |
| `/export` | `[path]` | Save the conversation as Markdown. With no path, choose a default file name or the clipboard. |
| `/files` | | List files read or changed by session tools, plus the working directory's Git status. Read-only. |
| `/fork` | | Copy completed turns into a new session and switch to it. The original stays in `/resume`. |
| `/goal` | `[--budget USD] <objective>`, `pause`, `resume`, `clear` | Set, view or control a goal. See [Goals](#goals). |
| `/handoff` | `[direction]` | Write a self-contained note from the current context into a new session's input box. See [Handoff](#handoff). |
| `/help` | | Show the shortcuts and commands. |
| `/init` | | Ask the model to create or improve `AGENTS.md`, or `CLAUDE.md` when no `AGENTS.md` exists, for this project. |
| `/lsp` | | Show language servers and their state. |
| `/mcp` | | Show MCP connections and tools. |
| `/memory` | | View, edit and delete global and project memories. |
| `/mode` | `[always-ask\|ask-for-execution\|allow-all]` | Change the permission mode. See [Permissions](#permissions). |
| `/model` | `[<provider>/<model>]` | Choose a provider and model, or switch directly. See [Models](#models). |
| `/new` | | Start a new session. |
| `/permissions` | | View and delete command rules and file rules. |
| `/plan` | | Enter or leave Plan mode. |
| `/ps` | | List background commands; open a command's output or stop it. |
| `/rename` | `[name]` | Rename this session; omit the name to clear it. |
| `/resume` | | Choose another session with the same picker as `canter --resume`. |
| `/review` | `[PR number or URL]` | Review uncommitted changes, or a GitHub PR through `gh`, as a read-only turn. See [Review](#review). |
| `/rewind` | | Rewind conversation, files, or both, to before a chosen message. See [Rewind](#rewind). |
| `/roles` | | List built-in and custom sub-agent roles. See [Sub-agents and roles](#sub-agents-and-roles). |
| `/sandbox` | | Show what the sandbox allows. Read-only. |
| `/skills` | | List this session's skills, grouped by global and project. |
| `/statusline` | | Choose which status line items appear. Saved to `tui.json`. |
| `/stop` | | Stop all background commands in this session. |
| `/theme` | | Choose the code colour theme. Saved to `tui.json`. |
| `/think` | `[none\|low\|medium\|high\|xhigh\|max]` | Change the thinking effort for the current model. See [Models](#models). |
| `/usage` | | Show this session's token usage and cost by model. |

A skill can be invoked as `/<skill-name> [text]`. Skills share completion with built-in commands; a built-in command keeps a colliding name. See [Skills](#skills).

### Approvals and questions

When a tool needs a decision, a box appears above the input.

- ↑/↓ selects an option, Enter chooses it, and `1`–`9` choose by number. Esc interrupts the turn. Backspace or ↑ returns from writing your own answer to the options while the answer is empty.
- A command approval offers accept once, accept for this session, always allow (which writes a prefix rule to `rules.json` when canter can propose one), decline, decline with feedback, and cancel. The feedback is passed back to the model as the reason for the refusal; the turn continues.
- A `declineWithFeedback` answer becomes a tool result of the form `Tool call declined. The user says: ...`.
- A question from `ask_user` shows two to four options and also accepts a written answer.
- A Plan approval box shows the complete plan and three choices: implement with a selectable permission mode, clear the context and implement in a new session with the same directory, model and thinking effort, or keep planning with optional feedback.

Approvals are per call unless you choose a session grant or a permanent rule. A session grant lasts only in that session; a permanent rule is global and editable with `/permissions`.

### Review

`/review` reviews staged and unstaged changes plus the list of untracked files. `/review 123` and `/review https://github.com/owner/repo/pull/123` ask `gh` to read a GitHub pull request without checking it out. The review runs as a queued turn in this conversation; only its title appears as the prompt, and the answer stays in history.

The review turn is read-only regardless of permission mode or session grants: file changes and shell commands outside the existing read-only whitelist are denied. Diff content is capped at 256 KiB with statistics and a truncation notice, and collection times out after 15 seconds. Empty changes or collection errors send no message. The client checks the server's `review` capability first; if it is missing, the client asks you to run `canter serve --replace`. Untracked file names have their own bounded section ahead of truncated diffs; PR metadata and collected content are enclosed in explicit untrusted-data markers.

### Rewind

`/rewind` lists turns after the latest successful compaction. Choose a message, then choose `Conversation and files`, `Conversation only` or `Files only`. A preview lists each file as `will restore` or `will delete`; files changed outside canter since the turn are marked and require confirmation. If the session changed since the preview, the rewind conflicts and you can preview again. The selected message returns to the input box.

### Handoff

`/handoff [direction]` writes a self-contained note from the current context, creates a new session with the same directory, model, thinking effort and permission mode, and puts the note in the input box. It is never sent automatically; edit it and press Enter. If you change your mind before sending anything in the new session, press Esc twice within about two seconds to return to the original session and drop the note. The empty new session remains in `/resume`. Usage is charged to the original session.

`/handoff` needs an idle conversation with history and no `btw` answer running. Esc stops generation; an error leaves you in the original session.

### Btw

`/btw <question>` asks the model one question with the current context, even while the main turn runs. Its answer streams only in the terminal that asked, in gray; neither question nor answer enters the saved conversation. Usage counts toward session cost, but context occupancy does not change. One btw answer may run per session at a time; Esc cancels it before it interrupts the main turn.

## Models

### Choosing a model

`/model` opens a provider list when more than one provider is configured; Enter opens that provider's models. The model list shows each model's window and whether it accepts images; the current one is marked. Esc goes back one level and then closes. With only one provider, `/model` goes straight to its models. `/model <provider>/<model>` switches directly.

Switching is allowed while the session is idle. The conversation history is sent as-is to the new model; if it exceeds the new model's compaction line, canter compacts first. A model is never switched automatically.

Changing the model with `/model`, or the effort with `/think`, records the session's resulting model and thinking effort for future sessions. Only an update that actually changes one of them is recorded, and only from a non-sub-agent session. An explicit `-m` (including `canter --resume -m`, `canter -c -m` and `canter exec --resume -m`), creating a session, opening an existing session, renaming it and starting a sub-agent do not change what is remembered. When several sessions make a valid choice, the last one wins.

A session created without an explicit model (a fresh `canter`, or a client that leaves the model empty) uses the remembered pair. If the remembered model is no longer available — including from the signed-in ChatGPT catalog — canter uses `model` from `config.json` and that model's default effort. If only the remembered effort is unsupported by the chosen model, that model's default effort is used. An explicit `-m` uses that model and its default effort, not the remembered effort; an explicit thinking effort still wins. A missing, unreadable or malformed `last_model.json` counts as no selection; an unreadable or malformed file also logs one server-side line. `/new` keeps the current session's model, thinking effort and permission mode; `/fork` inherits them from the source session; `/handoff` copies them into the new session.

### Thinking effort

`/think` lists the levels the current model supports; `/think <level>` sets one directly, and the change is remembered for future sessions. A new session uses the remembered effort when the chosen model supports it; otherwise it uses the model's `default_reasoning_effort`. If the current level is not supported after a model switch, canter uses the new model's default and shows a warning. An explicitly requested unsupported level is rejected rather than replaced.

The status line shows the server's current setting. For Chat Completions providers, canter leaves the provider-specific spelling to the adapter; for ChatGPT it sends the selected `reasoning.effort`.

### ChatGPT

`canter auth login chatgpt` signs in with the official flow. The request path is fixed to the official Responses endpoint; the provider object uses `"api": "responses"` and `"auth": "chatgpt"` and must not contain a key or base URL. After sign-in, the account's model catalog supplies models and details that are not hand-written.

Requests are `store: false`, streamed, and send the full input each time. The selected thinking effort is sent. If the plan quota is exhausted, canter reports the usage link and does not retry. A ChatGPT-plan request is recorded as plan usage: tokens count, but it adds no dollars and does not consume a goal's dollar budget.

Use `canter auth use chatgpt <email|number>` to choose among local accounts, `canter auth status` to inspect them, and `canter auth background chatgpt on|off` to allow or stop tasks after the last terminal closes. Without consent, new requests pause when the last session view disconnects; running tools finish and save.

### Images

Attach an image with Ctrl+V (macOS clipboard), or paste a local image path into the input. canter accepts JPEG, PNG, GIF and WebP up to 8 MiB each, identified by content, and keeps a private copy in the session directory. Images are sent to the model as image input when the current model accepts them. Switching to a model without image support substitutes `[image omitted: name]` in that request; saved history is unchanged.

A request sends at most 40 MiB of recent image data (base64 size, newest first); older images become `[image omitted: name]` for that request only. The `read` tool can also open images and PDF page images.

### Usage and cost

`/usage` shows this session's cached-input, uncached-input, output tokens and cost per model, the total, and the cost of the last completed turn. `canter usage` aggregates all sessions by local date and model. The status line shows the session cost when known; `+?` means some requests had no price. Amounts are USD; a small positive amount is shown as `<$0.01`, other values with two decimals.

Prices are selected by request start time from the built-in table or `prices`. Thinking tokens are included in output. Cache writes are not charged. Interrupted or failed requests that never received usage are not counted and may understate the total.

### Retries

Rate limits, 5xx/overload, network errors and timeouts are retried up to `model_max_retries` times (default 10). Backoff starts at 0.5 seconds, doubles to at most 32 seconds, and includes jitter; `Retry-After` is honored up to 60 seconds. Authentication errors, invalid requests and context-length errors are not retried. Esc cancels the wait immediately.

## Permissions

Three permission modes control when canter asks:

| Mode | Edits | Non-read-only commands |
| --- | --- | --- |
| `always-ask` | Ask | Ask |
| `ask-for-execution` | Free | Ask |
| `allow-all` | Free | Free |

New interactive sessions start in `always-ask`. `canter exec` starts in `ask-for-execution`. `/mode` without an argument opens a chooser; `/mode <name>` sets it directly. Shift+Tab cycles the three modes, then Plan.

Read-only shell commands that match canter's whitelist run without approval in the two asking modes. The whitelist covers listings, file views, searches and read-only Git inspection, including each segment of a compound command. In those modes, a path written in a command that lands in canter's state directory, `~/.ssh`, `~/.aws`, `~/.gnupg` and similar protected locations is still asked about. While the sandbox is on, the file tools cannot read or write those protected locations at all. This is a set of heuristics, not a sandbox: shell variables, symbolic links and program-internal reads are not fully traced.

`--yolo` at startup selects `allow-all` and disables the sandbox for this open. Shift+Tab switching to `allow-all` does not disable the sandbox.

### Permanent rules

`rules.json` stores decisions that survive sessions:

```json
{
  "rules": [
    { "pattern": ["cargo", "test"], "decision": "allow" },
    { "pattern": ["rm", "-rf"], "decision": "forbidden" }
  ],
  "fileRules": [
    { "pattern": ".env", "decision": "forbidden" },
    { "pattern": "./secrets/**", "decision": "prompt" }
  ]
}
```

- Command `pattern` is an array of words and matches a command prefix. `allow` skips the question; `prompt` always asks; `forbidden` blocks and tells the model. When several rules match, `forbidden` beats `prompt`, which beats `allow`.
- `fileRules` controls only the model's `read` (including images), `write` and `edit`. A pattern without `/` matches the file name; one with `/` is relative to the session directory, and `~/` is relative to the session HOME. Globs (`*`, `?`, `[...]`, `{...}`) are supported; patterns must not end in `/` or contain `.`/`..` components. The same decision precedence applies.
- canter's dangerous-command checks run before these rules and cannot be waived by them.
- Rules only affect model tools: user `!` commands, MCP tools, web fetch and web search keep their own approval rules.
- The file is re-read on every check, so edits need no restart. A missing file means no rules. An unreadable or invalid file falls back to asking once for model shell commands.

`/permissions` lists both kinds and lets you delete entries. Approving "always allow" in a command approval writes a prefix rule when canter can derive one.

### Sandbox

The sandbox is a separate switch from the permission mode. It is on by default (`"sandbox": true`) and uses macOS Seatbelt. It constrains model shell commands, including foreground, background and temporarily written scripts:

- Writable: the session working directory, directories added with `--add-dir` or `/add-dir`, the session's private temporary directory (also `$TMPDIR`, where truncated command output is saved so the model can read the middle), and a disposable cache area for Cargo and npm. The `.git` parts needed for commits are writable, but Git config and hooks are locked.
- Not readable or writable: the canter state directory, the control socket, `~/.ssh`, `~/.aws`, `~/.gnupg` and similar credential locations. The whole `/tmp`, the real `~/.cargo` and the real `~/.npm` are not opened.
- `allow-all` with the sandbox on allows network access but does not expand the write range; a command that steps outside fails rather than prompting for a wider range.
- `ask-for-execution` and `always-ask` can ask once for a command to use the network (all IP networks for that command and its children) or to write specific paths outside the range. The request names a justification and canonical paths. A rule, session grant or read-only whitelist does not move a command outside the sandbox; it only removes canter's own approval, not the sandbox boundary.
- The file tools `read`, `write` and `edit` follow the same boundaries while the sandbox is on.
- User `!` commands (including `!command &`) are not sandboxed, and MCP and `web_fetch` run outside it. Their cards say `not sandboxed`.
- A model can request network or extra write paths for one command with `sandboxRequest`; this is shown as a sandbox approval. It is per command, never permanent.
- If the sandbox cannot start, the command fails and is not retried outside it. A blocked command is not retried with wider permissions automatically.
- `sandbox: false` restores the old behavior; approvals still apply. The session temporary directory is still provided.
- `/sandbox` shows the effective ranges. The status line shows `Sandbox on` or `Sandbox off` after the permission mode when the server supports it.

`--yolo` disables the sandbox for that open, including `/new`, `/resume`, forks, handoffs and reconnects in the same terminal. Reopening the session later without `--yolo` restores the configured sandbox state. Child agents inherit the sandbox state of the parent at creation.

### Folder trust

Trusting a folder prevents project settings or language servers from executing code merely by opening a repository. It is not a permission mode and does not replace tool approvals or the sandbox.

- Trust is recorded for the canonical Git repository root. Worktrees that correctly point back to the main repository share its trust; submodules and nested repositories are separate.
- A trusted working directory does not automatically trust every added directory: each file's own repository root (or the covering added directory) is checked for LSP. A restricted session starts no LSP anywhere.
- A restricted session still loads `AGENTS.md` and skills, and global `config.json` MCP, rules and sandbox settings still apply; hooks and LSP are disabled. The terminal shows `Restricted mode: LSP and hooks are disabled.`
- `canter exec` opens an untrusted folder restricted and prints the same warning on stderr. It never asks.
- There is no command to list or revoke trust; the record is `trusted_folders.json`.

## Tools

The model sees only tools that the current provider and configuration support. Tool results are bounded; a failure is returned to the model as text and the turn continues.

| Tool | Purpose |
| --- | --- |
| `read` | Read a UTF-8 text file with line numbers, at most 2000 lines and 64 KiB per call. Reads images by content; reads PDF text, or one PDF page as an image with `page` (page images require `pdftoppm` from Poppler). |
| `ast_grep` | Search code by syntax structure with one ast-grep pattern, such as `$X.unwrap()`, so comments and strings do not match. Languages: Rust, Python, TypeScript (including `.tsx`), JavaScript (including `.jsx`, `.mjs`, `.cjs`). Directories skip `.gitignore` entries; at most 50 matches per page, with `offset` for the next page. |
| `write` | Create or replace a whole UTF-8 file; creates missing parent directories. |
| `edit` | Replace exact text in an existing UTF-8 file. All edits in one call succeed or nothing is written. |
| `apply_patch` | Apply a freeform `*** Begin Patch` patch. Offered only when the model's catalog entry supports the freeform patch tool; other models use `write` and `edit`. |
| `shell` | Run `/bin/sh -c` in the session directory. Foreground timeout defaults to 120 seconds, maximum 600; keep the beginning and end of output. Above 32 KiB the whole output is also saved under the session's temporary directory (up to 8 MiB per call), and the result names that file so `read` with `startLine` or `rg` can show the middle; the path works only while the session is open. `background: true` returns an id and initial output. |
| `shell_output` | Read output added since the last model read from a background command; reports running state and exit code. |
| `shell_stop` | Stop a background command's whole process group and return the remaining output. |
| `monitor` | Watch a background command's new output for a regular expression and wake or notify the model. Available in Plan mode. |
| `web_fetch` | Fetch one HTTP(S) URL with a 30-second timeout and a 5 MiB download limit; HTML becomes Markdown, other text stays text. Follows redirects only on the same host and port; needs a new approval otherwise. No credentials or cookies. |
| `web_search` | Search the web with 1–128 character queries and up to 20 results. Only offered when a configured provider uses the official `https://api.deepseek.com` root; uses that provider's key. |
| `mcp_search` | Find MCP tools in the session catalog. No approval needed; also available in Plan mode and review turns. |
| `mcp_call` | Call an MCP tool by full name after reading its schema. Uses the target tool's approval rules; unavailable in Plan mode and review turns. |
| `ask_user` | Ask one multiple-choice question with two to four options. The user can also write an answer. |
| `update_plan` | Replace the session checklist with steps and `pending`/`in_progress`/`completed` statuses. At most one step is in progress. |
| `submit_plan` | Submit a complete Markdown implementation plan while in Plan mode. |
| `get_goal` | Read the current goal, status, dollar budget and spending. |
| `update_goal` | Mark a goal `complete` or `blocked` after verification. Only the user can create, pause, resume or clear a goal. |
| `spawn_agent` | Start a sub-agent with a fresh conversation. |
| `wait_agent` | Wait for one or more sub-agents to finish, fail, close or time out. |
| `send_input` | Send a follow-up to an existing sub-agent. |
| `close_agent` | Close a sub-agent, cancelling its active turn and stopping its background commands. |

When MCP tools are present and `mcp_tool_search` is true (the default), the model gets `mcp_search` and `mcp_call` instead of every MCP tool definition. With `false`, all MCP tools are listed directly. Tool definitions follow configuration reloads at the next turn boundary.

`web_fetch` and `web_search` are treated as execution: the asking modes prompt, `allow-all` runs them, and review turns deny them. Session grants for `web_fetch` cover one canonical host and port; search grants cover the session's searches.

`ast_grep` is read-only and available in every permission mode, in Plan mode, in review turns and to read-only sub-agents. Its path follows the same `rules.json` file rules and canter private-file refusal as `read`; a directory search applies those rules to every file, follows symlinks the same way, and skips files that are blocked, need approval or cannot be read, reporting how many were skipped.

## Hooks

Hooks run commands at fixed points in a turn. They are configured only in the global `<CANTER_HOME>/config.json`; project hooks are not read. Restricted sessions run no hooks.

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "write|edit",
        "hooks": [
          { "type": "command", "command": "cargo fmt", "timeout": 60 }
        ]
      }
    ]
  }
}
```

Only `type: "command"` is supported. `timeout` is a positive number of seconds, default 60. For `PreToolUse`, `PermissionRequest` and `PostToolUse`, `matcher` is a regular expression over the model's full tool name (`read`, `write`, `edit`, `shell`, `ask_user`, `web_fetch`, `mcp_call`, and so on); an empty matcher, no matcher or `*` matches all. Other events ignore the matcher. An unknown event, invalid regex, empty command, non-positive timeout or malformed structure rejects the whole configuration; the last valid configuration stays active with a warning.

Every hook command runs with `/bin/sh -c` in the session working directory. A JSON object is written to stdin with `session_id`, `cwd`, `hook_event_name` and `permission_mode` (`alwaysAsk`, `askForExecution`, `allowAll` or `plan`), plus event-specific fields. The environment is the session's environment with `CANTER_SESSION_ID` and `CANTER_PROJECT_DIR` set by canter. Hooks are not sandboxed.

| Event | When | Effect |
| --- | --- | --- |
| `SessionStart` | A session is created or reopened; also after a client leaves and a new event stream reconnects. `source` is `startup` or `resume`. | Notification. |
| `SessionEnd` | The last client has been away for 10 seconds and the session is idle; explicit close; normal server shutdown. `reason` is `client_exit`, `client_request` or `server_shutdown`. | Notification. |
| `UserPromptSubmit` | Before a user input or queued input is processed, and before a steer is consumed. `prompt` is the text. | May block the input; the model and history never see it. |
| `PreToolUse` | After canter's own rules and before the approval box. `tool_name`, `tool_input`. | May block; the reason becomes the tool result and no approval is shown. |
| `PermissionRequest` | A tool is about to wait for user approval. | Notification only; cannot answer for the user. |
| `PostToolUse` | A tool that actually ran returns, successfully or not. `tool_name`, `tool_input`, `tool_response`. | Feedback is appended to the result; cannot undo it. Not run for blocked, declined or preparatory failures. |
| `Stop` | A turn ends normally. `last_assistant_message`. | Notification. |
| `StopFailure` | A turn ends with an error. `error`. | Notification. |
| `Interrupt` | A turn is interrupted. | Notification. |
| `SubagentStart` | A sub-agent is created, before its first turn. `agent_id`, `agent_name`, `agent_type`, `parent_session_id`, `task`. | Asynchronous notification. |
| `SubagentStop` | A sub-agent turn completes, fails, is interrupted, or is closed while running. `agent_id`, `agent_name`, `agent_type`, `parent_session_id`, `status`, `last_assistant_message`. | Asynchronous notification. |

Exit code `0` allows. Exit code `2` blocks or provides feedback, using stderr as the reason. A successful exit can also print JSON: `{"decision":"block","reason":"..."}`, or for `PreToolUse`, `{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"..."}}`. Other exit codes, timeouts, launch failures, and non-JSON output allow the action and produce a warning. Hook output never enters the model context except a block reason or PostToolUse feedback. After a hook runs, canter re-checks the tool's own rules, sandbox and Plan/review restrictions; a hook cannot widen them.

## MCP

MCP servers are configured in canter's own `config.json` under `mcp_servers`, or with `canter mcp ...`. Other agents' MCP configurations are never read.

```json
{
  "mcp_servers": {
    "local": {
      "transport": "stdio",
      "command": "/absolute/path/to/mcp-server",
      "args": ["--stdio"],
      "env": { "SERVICE_KEY": "YOUR_KEY" }
    },
    "remote": {
      "transport": "http",
      "url": "https://mcp.example.com/mcp",
      "headers": { "Authorization": "Bearer YOUR_TOKEN" }
    }
  }
}
```

- stdio runs the command directly, without a shell. The child environment starts empty, then receives only `HOME`, `PATH`, `USER`, `LOGNAME`, `SHELL`, `TMPDIR`, `LANG` and `LC_ALL` from the server, plus the configured `env`. `CANTER_HOME` is not inherited.
- HTTP uses Streamable HTTP. Plain HTTP is allowed only for localhost or loopback; other URLs must be HTTPS. Redirects and proxies are not used.
- `startup_timeout_sec` (default 10) limits connection, initialization and tool listing; `tool_timeout_sec` (default 120) limits one call. Both accept integer seconds from 1 to 86400.
- Environment values and headers stay in the private config and running memory. They are not shown in `/mcp`, events, logs or session files; known values are redacted from tool text when at least 8 bytes long.

MCP tools appear as `mcp__<server>__<tool>` when listed directly; the model normally sees `mcp_search` and `mcp_call`. Every MCP call is treated as execution and follows the normal approval rules. A session grant covers one server and tool. Review turns deny MCP calls without asking.

### Playwright for web testing

The Playwright MCP server drives a real Chromium browser, so the model can open a page, click, fill in forms and check the result without a built-in browser tool. Add it to `mcp_servers`:

```json
{
  "mcp_servers": {
    "playwright": {
      "transport": "stdio",
      "command": "/path/to/npx",
      "args": ["-y", "@playwright/mcp@0.0.83", "--isolated"],
      "env": {
        "PATH": "/path/to/node/bin:/usr/local/bin:/usr/bin:/bin",
        "HOME": "/path/to/home"
      }
    }
  }
}
```

- Pin a specific version (`0.0.83` is the version tested here); do not use `@latest`, so the tools and behavior do not change under you.
- `--isolated` starts a blank browser profile each time. It has none of your normal Chrome login state or cookies.
- The server starts `npx`, and `npx` needs `node`. The stdio child environment is cleared and then receives only a few base variables plus `env`, so put the directory containing `node` in `PATH` and set `HOME` as shown.
- After saving the configuration, open a new session (or let a running server pick it up at the next turn). `/mcp` should show `playwright connected` and its tools.
- In the asking permission modes, every Playwright tool needs approval on its first use. You can choose to allow that tool for the session; the grant is per tool, so one approval does not allow the other Playwright tools.
- Models that cannot see images can still use it: Playwright reads the page structure, such as buttons, fields and text. When the model asks for a screenshot, only models that accept image input can see it.

For OAuth services:

```sh
canter mcp add gitlab --url https://your-gitlab.example/api/v4/mcp
canter mcp login gitlab
canter mcp logout gitlab
```

`login` opens the browser and listens on a temporary `127.0.0.1` port for up to five minutes. Tokens are stored in `<CANTER_HOME>/mcp-auth/` (0700 directory, 0600 files), bound to the service URL. If a service needs authorization again, `/mcp` says `Login required: run canter mcp login <name>`. A running server picks up MCP configuration changes at the next turn; `canter serve --replace` is the fallback if a service cannot be handed over, and `canter mcp add` also prints that advice.

## Skills

Skills are Markdown instructions kept outside the repository or inside it:

- Global: `~/.agents/skills/<name>/SKILL.md`
- Project: `<project root>/.agents/skills/<name>/SKILL.md`

The file starts with YAML front matter:

```markdown
---
name: my-skill
description: What this skill is for, shown in completions and /skills.
disable-model-invocation: false
---
Instructions for the model.
```

`name` and `description` are required and must be non-empty. `disable-model-invocation: true` keeps the skill out of the model's own skill list but still lets the user invoke it. canter reads only these two directories, not `~/.claude/skills` or `~/.codex/skills`.

`/skills` lists the skills frozen when the session opened, grouped by source. Type `/<skill-name> [text]` to invoke one; the full `SKILL.md` is read at that point and inserted into the model request, while the visible conversation keeps only the skill name and your text. Changing a skill file affects later messages that invoke it, not a skill invocation already sent. Reopen the session to pick up a new skill in the list.

`canter skills install` copies the bundled `docx`, `pptx`, `web-testing` and `xlsx` skills to `~/.agents/skills/`. `--force` overwrites only their `SKILL.md` files. The document skills teach the model to inspect and write Office files with available local tools; canter itself adds no Office parser libraries. The web-testing skill teaches the model to choose between headless Playwright scripts and Playwright MCP. Browser scripts cannot run while the sandbox is enabled: use `canter --yolo` for a session that needs headless scripts, or use Playwright MCP instead.

## Sub-agents and roles

canter can start sub-agents in separate sessions of the same server. They have their own conversation, events and usage, and their cost counts toward the parent's goal.

- `explorer` is read-only: it uses the Plan-mode tool set without `ask_user` or `submit_plan`, may use `mcp_search`, may not call MCP tools, and keeps its own `update_plan` checklist.
- `general` gets all tools except `ask_user` and the four sub-agent tools. It is intended only when the user or `AGENTS.md` explicitly asks for delegation or parallel work.
- At most four sub-agents may run at once, including unfinished background work. A sub-agent cannot start another sub-agent.
- `/agents` lists the parent's direct sub-agents; open one to view it or stop a running one. Sub-agents are excluded from the normal session list and `canter stats` list, but their usage is included in totals.

Custom roles are Markdown files in `<CANTER_HOME>/agents/*.md` (not recursive, not project files):

```markdown
---
name: reviewer
description: Review recent changes for bugs.
tools: read, shell
model: provider/model
effort: high
---
Report issues that cause incorrect behavior.
```

`name` and `description` are required. `name` is 1–64 characters of lowercase ASCII letters, digits or `-`, starts with a letter, and cannot be `explorer`, `general` or `unknown`. `tools` is a comma-separated list of canter model tool names; it can only narrow the `general` tool set, and `ask_user` and the sub-agent tools are not available. `model` is `<provider>/<model>`; `effort` is one of the thinking levels and must be supported by the target model. If both `model` and `effort` are omitted, the parent's model and effort are inherited; if only `model` is given, the target model's default effort is used; if only `effort` is given, it is checked on the parent model. The body is additional instructions.

The role directory is read at the start of each parent turn; changes affect later spawns, not existing sub-agents. A broken file disables only that role, with an English reason in `/roles`. `/roles` shows built-in and custom roles with their model, read-only/edit status and description.

## Memory

canter keeps durable notes in two layers:

- Global: `<CANTER_HOME>/memory/*.md`
- Project: `<CANTER_HOME>/projects/<key>/memory/*.md`; all worktrees of one repository share the project layer.

Each file is Markdown with a first line `# Name`, a non-empty one-line description on the second line, a blank line, then the body. Only the header is loaded into a session's context as an index; details are read on demand. Up to 200 lines and 25,000 bytes are included, with a truncation notice.

The model writes and edits memories with the normal `write` and `edit` tools; these two memory directories do not need approval. `/memory` lists name, description and layer; Enter opens the file in `$VISUAL` or `$EDITOR`, and `d` asks once before deleting it. Restricted sessions neither read nor write memories. There is no background extraction, extra model request, vector search or automatic cleanup.

## Long-running tasks

canter adds a few bounded checks during ordinary task execution:

- After three consecutive tool calls with identical names, argument bytes, and result text, the model receives a reminder to try another approach. If four results are identical and it requests the same call again, canter stops before executing the fifth call and shows a warning. An active goal is paused. Send a new message to continue.
- When the model stops with unfinished plan steps, canter reminds it to continue and shows a warning, up to three times per turn. The reminder allows it to explain a blocker or ask for a user decision and stop. This also applies to sub-agents; active goals use their existing continuation instead.
- When the model reports an output length limit, canter asks it to continue from the cutoff. Truncated tool calls are not executed; the model is asked to split them into smaller steps. Text and tool continuations share a limit of three per turn, after which canter stops with the existing incomplete-reply warning.

These checks are disabled in Plan mode, reviews, and the final goal-budget summary. Esc stops continuation, and steering input is handled first. A new turn or user steering resets the counts; compaction does not. Ordinary turns that do not trigger a check send no extra requests or reminders.

## Plan mode

Plan mode is a session setting, not a fourth permission mode. Enter or leave it with `/plan` or by cycling modes with Shift+Tab (the cycle includes `Plan`). It is saved with the session and restored by `/resume` and `-c`; only the user can enter or leave it. While in Plan mode:

- Read-only tools are allowed: `read`, read-only whitelisted `shell`, `shell_output`, `shell_stop`, `monitor`, `web_search` and `web_fetch` with their normal approval rules, and `mcp_search`, plus `ask_user` and `submit_plan`.
- `write`, `edit`, `apply_patch`, other shell commands, `update_plan`, `mcp_call` and every other MCP tool are blocked with an English explanation, and no approval box appears.
- The model is told to investigate, clarify open questions with `ask_user`, and submit a complete Markdown plan with `submit_plan`.
- The status line shows `Plan (then <mode>)`, the permission mode that will apply after leaving Plan.

Submitting a plan opens an approval box with the full plan. `Yes, implement this plan` leaves Plan and continues the turn with the selected permission mode. `Yes, clear context and implement` ends the original turn, starts a new session in the same directory with the same model and thinking effort, and sends the plan and an implementation instruction as its first message. `No, keep planning` accepts optional feedback and stays in Plan. With `plan_enabled` false, the `update_plan` tool is not offered.

## Goals

A goal keeps the model working toward a long-running objective and tracks spending.

```text
/goal [--budget USD] <objective>
/goal
/goal pause
/goal resume
/goal clear
```

`/goal <objective>` creates a goal and starts it immediately. The budget defaults to `goal_budget_usd` from `config.json` (default 5 USD); `--budget 0` means unlimited. Only a user can create, pause, resume or clear a goal; an unfinished goal refuses to be overwritten until `/goal clear`.

While a goal is active, each turn appends the objective, spending and remaining budget, plus instructions to report progress and verify completion. The goal continues automatically after a turn ends, independently of the background-wake limit. Esc pauses it; a normal message is processed first and then the goal resumes only if it is still active. A paused goal resumes only with `/goal resume`. Goal state and spending are saved in the session; rewinds do not refund spending.

With a non-zero dollar budget, reaching 90% or recording a request with unknown cost makes the goal `budget-limited`; canter sends at most one summary request, which does not run new tools. With budget 0, unknown cost is marked but does not stop the goal. ChatGPT-plan requests do not count toward the dollar budget. A model request that ultimately fails stops the automatic continuation and keeps the goal so you can resume later. Goal continuation questions to `ask_user` time out after five minutes and tell the model to decide and disclose it.

## Sessions

canter saves every session on the server, independent of any terminal.

| Action | How |
| --- | --- |
| New | `canter` (optionally with a first prompt) or `/new`. `/clear` is another name for `/new`. |
| Resume | `canter --resume [id]`, `canter -r [id]`, or `/resume`. Without an ID, the picker opens. |
| Continue this directory | `canter -c`. |
| Fork | `/fork` copies only completed turns and their settings, adds ` (fork)` to a non-empty name, and switches to it. The original stays open and is available in `/resume`. |
| Rename | `/rename [name]`; omitting the name clears it. |
| Search | In the session picker, `/` searches message text; Tab switches this directory/all directories. |
| Trash | In the session picker, `d` asks once, then moves the session to `trash/`. A session with a connected client, running turn, pending approval or question, queued input, goal continuation, running background command or btw answer cannot be trashed; stop it first. |
| Export | `/export [path]`. Without a path, choose a file named `canter-<session name>-<time>.md` in the session directory, or copy the Markdown to the clipboard. |
| Copy answer | `/copy [N]` copies the last answer, or its code block N. |
| Inspect context | `/context` shows categories, estimated tokens, the window, the compaction line and the last measured usage. |
| Inspect files | `/files` lists files read or changed by tools and the repository's Git status. Shell edits appear only in Git status. |
| Inspect diff | `/diff` shows uncommitted Git changes. |
| Compact | `/compact` summarizes now; otherwise canter compacts automatically at the model's compaction line. |

Conversation compaction keeps the system prefix and a summary, restores a few recently read text files, and preserves the current plan, goal and invoked skills. File changes and shell edits are not rolled back by compaction.

Sessions are cleaned automatically: at server readiness and then every 24 hours, sessions and trash entries whose last event is strictly older than `session_retention_days` are removed. Sessions loaded in the server, or with running work, are kept. Usage from removed sessions is summarized into `usage-archive.jsonl` so `canter usage` totals do not change. A parent and its child sessions are cleaned as a family.

## Language servers

LSP is off by default. Turn it on in `config.json`:

```json
{
  "lsp": {
    "enabled": true,
    "servers": {
      "rust-analyzer": {
        "command": "rust-analyzer",
        "extensions": { "rs": "rust" },
        "root_markers": ["Cargo.lock"]
      }
    }
  }
}
```

Built-in server definitions cover Rust (`rust-analyzer`), Python (`pyright-langserver`) and TypeScript/JavaScript (`node_modules/.bin/tsc --lsp --stdio`). A `servers` entry merges over the built-in one by id. Server ids may contain ASCII letters, digits, `-` and `_`. `command` must name an installed server, not a package runner such as `npx`; `extensions` maps file extensions to language ids; `root_markers` finds the project root; `initialization_options` and `settings` are passed through.

When a file is written by `write` or `edit`, canter waits up to 500 ms for diagnostics and appends them to the tool result when they arrive quickly; later diagnostics are delivered before the next model request. Only changed files and recently touched project files are checked, not an entire project. Language servers do not start in restricted sessions, Plan turns or review turns, and `/lsp` shows the current servers.

## Status line

Without a custom status command, canter draws three status rows under the input box:

```text
 ◆ opencode-go/deepseek-v4.1-flash · ◐ think high · ▲ Allow all · ▲ Sandbox off · $0.03
 ●○○○○○○○○○  48k/400k ( 12% to compact) · ⟳ cache  95% (last  99%) · ↯  41 tok/s
 ~/Developer/personal_projs/canter ⑂ main · +250 −0
```

- First row: the model, the thinking effort, the permission mode, the sandbox state and the session's cost. Dangerous settings, `Allow all` and `Sandbox off`, are red with a `▲` before them; the others are grey and have no mark.
- Second row: context use as a bar that is full at the auto-compaction line (`of window` when the server does not send the line), the cache hit rate for the session and for the last request, then the speed, the version and the session name when they are turned on. The bar's used cells go from green to yellow to red along it, by the room left where each ends; its percent is green, yellow or red by the room left. The numbers keep their width, so a changing number does not move what follows it. The row is left out while it has nothing to show, such as in a new session.
- Third row: the working directory, the git branch after `⑂`, and the lines added and removed since the last commit (left out when nothing changed; a count of 0 is grey). Numbers changing above never move it.
- Items start with a small coloured mark: `◆` model, `◐` thinking effort, `⟳` cache, `↯` speed, `⑂` branch. The model, the cost and the numbers are bold; the words around them are grey.
- Items are separated by ` · ` and keep their places. When the window is too narrow, each row drops whole items from its end; a long directory is shortened in the middle (`~/…/src/canter`), and the lines changed show only when they fit.
- The speed (`tps`) is in whole tokens a second: the text so far over the time from its first to its latest token, so it holds while the model waits, and the server's token count once the request ends. It changes at most once a second, and between responses shows the last one's.

Under the three rows, a row each for sub-agents, background commands and the current goal (`◎ goal $0.12 / $5.00`) appear when there are any. ↓ from the input box selects the sub-agents or background commands row (`· ↓ to view`), and Enter opens its list.

`/statusline` chooses which items appear; their places are fixed. The result is saved in `tui.json` under `statusLine`. Available items are `model`, `thinking`, `permission`, `cost`, `context`, `cache`, `tps`, `version`, `session`, `directory`, `branch` and `diff` (`tps`, `version` and `session` are off by default).

For a fully custom status line, add a terminal-only `statusLine` string to `config.json`:

```json
{ "statusLine": "~/bin/my-status" }
```

canter runs it as `/bin/sh -c <command>` and writes a JSON object to stdin whenever the display changes (at most about every 300 ms). The first two output lines take the place of the status rows, and the sub-agent, background command and goal rows stay under them; ANSI styling is allowed. The command is read when the terminal starts and is not hot-reloaded. `/statusline` can switch between the script and the built-in items; `useScript` in `tui.json` records the choice.

Other `tui.json` settings:

| Key | Default | Meaning |
| --- | --- | --- |
| `theme` | `terminal` | Code colour theme chosen by `/theme`. Custom `.tmTheme` files are read from `<CANTER_HOME>/themes/`. |
| `notifications` | `true` | `true`, `false`, or a list of `turn-complete`, `approval-requested`, `question`. |
| `notificationMethod` | `auto` | `auto`, `osc9` or `bel`. `auto` uses OSC 9 in Ghostty and a bell elsewhere. |
| `notificationCondition` | `unfocused` | `unfocused` or `always`. A terminal that never reports focus counts as focused. |

## Non-interactive mode

`canter exec [options] [prompt]` runs one turn and exits. It keeps the session, so the next run can resume it.

```sh
canter exec "summarize the failing tests"
canter exec --resume s-123 "continue the fix"
canter exec --last "what changed?"
printf 'review this diff' | canter exec -
canter exec --json "run the checks"
```

Options: `-m`/`--model`, `--add-dir`, `--tag key=value` (new sessions only), `--yolo`, `--resume <id>`, `--last`, `--json`. A prompt may be omitted or `-`, which reads stdin. If a prompt is given and stdin is a pipe, the stdin text is appended inside a `<stdin>` block. Without `-m`, a new exec session uses the model and effort remembered from `/model` and `/think`; `-m` applies to this run only and is not remembered.

The default permission mode is `ask-for-execution`, with the sandbox as configured. Approvals that would need a person are automatically declined with a reason, `ask_user` receives `No one is available. Use your own judgment and continue.`, and the model is told to proceed. `--yolo` uses `allow-all` and disables the sandbox for this run. If the server has `sandbox: false`, exec still runs and warns that commands needing approval will be declined.

Without `--json`, stdout is the final complete assistant message of the turn; tool traffic, automatic declines, retries, warnings and errors go to stderr. With `--json`, stdout is JSONL: `{"type":"exec.started","sessionId":"..."}`, then the public session events of the turn, then a final `{"type":"exec.completed",...,"outcome":"completed|failed|interrupted|budgetLimited|approvalDenied|error","exitCode":N,"usage":...,"cost":...}` line.

Exit codes: `0` completed, `1` client/argument/connection/protocol error, `2` the turn failed, `3` an approval was automatically declined, `4` the goal budget was reached, `130` interrupted by Ctrl+C, SIGTERM or SIGHUP. The precedence is 130, 2, 4, 3, 0; client errors are 1. Reasons are written to stderr.

## Files and privacy

- Credentials live only in `config.json` (mode 0600) and the private ChatGPT and MCP OAuth files. They are never written to session events, logs, test output or the repository.
- canter's state directory and configuration are protected from the model's `read`, `write` and `edit`; shell access is not fully contained by that protection, which is why the sandbox exists.
- Sessions store the public event log and a separate private model history. The private history is not served to clients.
- `telemetry` controls only local `telemetry.*` events; canter does not send telemetry anywhere.
