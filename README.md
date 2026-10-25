# canter

A coding agent written in Rust: one resident server per machine, many clients.

> **Status: design stage. There is no code yet.** This README describes what canter is meant to be. The decisions and the reasoning behind them are in [`docs/DESIGN.md`](docs/DESIGN.md). The research report that led to them is in [`docs/调研/R49-自研agent调研.md`](docs/调研/R49-自研agent调研.md). Both documents are in Chinese.

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
             └──────────── Codex app-server protocol (JSON-RPC over WebSocket) ───┘
                                          │
                              ┌───────────┴───────────┐
                              │     canter server     │   one per machine, resident
                              │  sessions · loop      │
                              │  tools · permissions  │
                              │  model providers      │
                              └───────────┬───────────┘
                                          │
                        ChatGPT (Responses API) · DeepSeek (Chat Completions)
```

- **Server.** The server holds the agent loop, tools, permissions and sessions. It starts in the background the first time a client cannot find it, and it can also be installed as a user service. Closing a terminal does not end the session. You can pick it up again from another terminal or client.
- **Clients.** Clients only show what the server sends and pass your input and decisions back. They never touch files or call models. Several clients can watch the same session.
- **Protocol.** canter uses the [Codex app-server](https://github.com/openai/codex) protocol rather than inventing a new one. A session holds turns, and a turn holds items. Approvals and questions are requests from the server to the clients.
- **Independent.** canter does not depend on any particular harness. Any tool that wants to drive it, such as a session manager or a chat bot, talks to the server like any other client.

## Planned first release

- Resident server with a terminal client that does not take over the full screen. The client reconnects and resumes after a server restart.
- Models:
  - ChatGPT, through "Sign in with ChatGPT", using your own plan.
  - DeepSeek, with your own API key.
- Tools: read, write, edit files, run shell commands.
- Permissions:
  - allow everything, ask every time, or allow for this session;
  - Esc stops an active `/btw` answer or `/handoff` generation first; otherwise it interrupts the current turn.
- Sessions are saved and can be resumed. Context is compacted manually or automatically when it gets too long.
- Reads `AGENTS.md`.
- `/context` shows the loaded context by category, token estimates, remaining window space and the last model-reported usage. `/files` lists files read or changed by session tools and the working directory's git status. Shell edits appear in git status, not in the session's changed-file list. Both commands are read-only.
- `/btw <question>` asks one question using the current context, even while the main turn runs. Its answer streams in gray in this terminal only; neither the question nor the answer enters saved history. Usage counts toward session cost, while context occupancy stays unchanged. Only one btw answer can run per session at a time.
- `/handoff [direction]` writes a self-contained note from the current context, opens a new session with the same directory, model, thinking effort and permission mode, and places the note in the input box. Edit it and press Enter to send; it is never sent automatically. Requires an idle conversation with no btw answer running. Esc cancels generation, and errors leave you in the original session. Usage is charged to the original session, which stays available through `/resume`.

The first goal is to be good enough for everyday small tasks. More planned: a GPUI desktop app, a web client, skills, MCP, sub-agents, and more model providers.

## Building

Not yet. canter will be a Cargo workspace on Rust stable:

```sh
cargo build
cargo test --all-targets
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
