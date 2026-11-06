# canter

A coding agent written in Rust: one resident server per machine, many clients.

> **Status: in daily use by its author on macOS. There are no prebuilt releases; build it from source.** The complete usage guide is [`docs/usage.md`](docs/usage.md). The decisions and the reasoning behind them are in [`docs/DESIGN.md`](docs/DESIGN.md). The research report that led to them is in [`docs/调研/R49-自研agent调研.md`](docs/调研/R49-自研agent调研.md). The design and research documents are in Chinese.

## What it is

A coding agent is the layer that connects a language model to your computer. It runs a loop:

1. It sends the conversation, file contents and command output to the model.
2. The model either answers in text or asks to call a tool, such as reading a file or running a command.
3. The agent runs the tool, after asking you first when the rules say so, and sends the result back to the model.

The turn ends when the model stops asking for tools. canter is that loop, plus a set of tools, permission rules and a session log, built to be small and readable.

## Architecture

```
        terminal UI      desktop app (GPUI)      web       other clients
             │                   │                │              │
             └────────── canter protocol (HTTP + SSE) ───────────┘
                                          │
                              ┌───────────┴───────────┐
                              │     canter server     │   one per machine, resident
                              │  sessions · loop      │
                              │  tools · permissions  │
                              │  model providers      │
                              └───────────┬───────────┘
                                          │
              ChatGPT (Responses API) · DeepSeek and other Chat Completions providers
```

- **Server.** The server holds the agent loop, tools, permissions and sessions. It starts in the background the first time a client cannot find it, and it can also be installed as a user service. Closing a terminal does not end the session. You can pick it up again from another terminal or client.
- **Clients.** Clients only show what the server sends and pass your input and decisions back. They never touch files or call models. Several clients can watch the same session.
- **Protocol.** canter has its own HTTP + Server-Sent Events protocol, structured like the [Codex app-server](https://github.com/openai/codex) v2 protocol. A session holds turns, and a turn holds items. Approvals and questions are requests from the server to the clients.
- **Independent.** canter does not depend on any particular harness. Any tool that wants to drive it, such as a session manager or a chat bot, talks to the server like any other client.

## Features

- **Resident server and terminal client.** One server per machine, many clients, with a full-screen terminal interface that reconnects and resumes after a server restart.
- **Models.** Several providers at once:
  - ChatGPT through "Sign in with ChatGPT", using your own plan.
  - DeepSeek and other OpenAI-compatible Chat Completions providers, with your own key or plan.
  - Switch provider and model while a session is idle; choose per-model context windows, image support, thinking levels and prices.
- **Tools.** Read files, images and PDFs; write or edit files; apply patches on supported ChatGPT models; run shell commands in the foreground or background; fetch web pages and search the web.
- **Permissions and sandbox.** Three permission modes, per-call and permanent rules, a macOS Seatbelt sandbox for model commands, and per-repository folder trust.
- **Sessions.** Saved and resumable; forked or rewound; searched across; moved to a trash; cleaned automatically after a configurable retention period. Context is compacted manually or automatically.
- **Project knowledge.** Reads `AGENTS.md`, skills and global and project memories.
- **Automation.** Hooks, MCP servers (stdio, HTTP and OAuth), sub-agents with custom roles, plan mode, goals and background commands.
- **Inspection.** `/context` shows loaded context by category, token estimates, remaining space and the last measured usage. `/files` lists files read or changed by tools plus Git status. `/review` reviews uncommitted changes or a GitHub PR as a read-only turn.
- **Side questions and handoff.** `/btw` asks one question without saving it to history; `/handoff` drafts a note in a new session.
- **Command line.** `canter exec` for one non-interactive turn, `canter stats` for session reports, `canter usage` for daily token and cost totals, and `canter doctor` for configuration, sign-in, provider, MCP and sandbox checks.

## Install

canter is a Cargo workspace on Rust stable. It is currently used on macOS.

```sh
git clone https://github.com/firegnu/canter.git
cd canter
cargo install --locked --path crates/canter
```

`canter upgrade`, `canter rollback` and the optional login service are built in:

```sh
canter upgrade              # rebuild from the source checkout, back up and install, then replace the server
canter rollback             # restore the previous build and replace the server
canter service install      # macOS: start one server at login; save this terminal's PATH and HOME
```

`canter upgrade [directory]` builds the current code from the checkout recorded at install time, or from a directory you pass. Both commands keep the five newest backups in `~/.canter/backups`. `service install` does not start or stop the server; it only writes the login item. See [`docs/usage.md`](docs/usage.md#server-and-service) for the exact wait and rollback behavior.

## Start using it

Create `~/.canter/config.json` with at least one provider. A minimal DeepSeek setup:

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

To use a ChatGPT plan instead, add a `responses` provider, set the model to `chatgpt/<model>`, then run `canter auth login chatgpt`. The usage guide has complete examples for [providers](docs/usage.md#provider-objects), [ChatGPT](docs/usage.md#chatgpt) and [prices](docs/usage.md#prices).

The server remembers the model and thinking effort you last chose with `/model` or `/think`; a newly started `canter` session uses them unless you pass `-m`. `config.json`'s `model` is the fallback when nothing is remembered or the remembered model is unavailable, and changing it clears the remembered selection. `-m` applies to that run only.

Then run canter in a project directory:

```sh
cd ~/code/my-project
canter
```

The first client starts the server in the background and opens a session. Before opening a folder canter has not seen, the terminal asks `Trust this folder?`; choose `Trust and continue` or `Open restricted`. Trust is recorded per Git repository root in `trusted_folders.json`. `canter exec` opens an untrusted folder restricted and never asks.

Settings live in `~/.canter/config.json`. Most changes are picked up at the next turn without restarting the server. Set `CANTER_HOME` to use another absolute state directory.

## Documentation

- [`docs/usage.md`](docs/usage.md): configuration, command line, terminal commands and keys, models, permissions, sandbox, tools, hooks, MCP, skills, sub-agents, memory, plan mode, goals, sessions, `canter exec`, statistics and doctor.
- [`docs/DESIGN.md`](docs/DESIGN.md): the decisions and their reasons (Chinese).

## Development

```sh
cargo build
cargo nextest run --all-targets
cargo test --doc --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Credits

The design borrows from these open-source coding agents:

- [Codex CLI](https://github.com/openai/codex) (Apache-2.0): protocol and server layout.
- [opencode](https://github.com/anomalyco/opencode) (MIT).
- [pi](https://github.com/earendil-works/pi) (MIT).
- [oh-my-pi](https://github.com/can1357/oh-my-pi) (MIT).

Code taken from other projects keeps its original license notice.

## License

[Apache-2.0](LICENSE)
