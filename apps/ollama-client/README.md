# ollama-client

A minimal chat client for a local [ollama](https://ollama.com) LLM server, for the
Xous / Precursor device. You type a prompt and read the reply; because replies can
be long, the whole transcript is **scrollable** — something the stock ShellChat
input can't do. The scrollable view is a custom `Framebuffer` UI, in the same
spirit as `apps/sidplayer`'s scrollable song list.

## Using it

1. Make sure the Precursor is on Wi-Fi and can reach the machine running ollama.
2. On the ollama host, bind it to your LAN, not just localhost:
   `OLLAMA_HOST=0.0.0.0 ollama serve` (and `ollama pull <model>` for a model).
3. Launch **Ollama Chat** from the app menu.
4. Press **F1** and enter the server **host** (IP or name) and **port** (default
   `11434`). These are saved in the PDDB.
5. Press **F2** to fetch the list of models installed on that server (ollama's
   `/api/tags`) and pick one. (You can also type a model name directly in the F1
   form if you already know it.)
6. Type a message and press **Enter**. The reply appears in the transcript.

Typing uses GAM's standard predictive text-entry line (the same input area as
ShellChat), so it's responsive. Because that input line also wants the arrow keys
(to move the text cursor), the arrows only scroll when you're in **scroll mode**,
which you toggle with **F3**. The title bar shows the current mode (`EDIT` /
`SCROLL`). After a reply that's taller than the screen, the app switches to scroll
mode automatically so you can read it straight away; sending a new message returns
to edit mode.

### Keys

| Key        | Action                                                   |
|------------|----------------------------------------------------------|
| type / ⌫   | edit the prompt line (always)                            |
| Enter (⏎)  | send the prompt (returns to edit mode)                   |
| F3         | toggle **edit** ⇄ **scroll** mode                        |
| ↑ / ↓      | *(scroll mode)* scroll the transcript one line           |
| ← / →      | *(scroll mode)* scroll one page                          |
| ↑ ↓ ← →    | *(edit mode)* move the text cursor in the input line     |
| F1         | server settings (host / port / model)                    |
| F2         | list the server's models and select one                  |
| F4         | display menu: toggle font size (Regular/Large) or clear   |

The input line uses a **no-op predictor** (no autocomplete bar to compete with the
function keys). F4 opens a small menu to switch the transcript between the Regular
and Large glyph size (the reply re-wraps to fit) or to clear the conversation. The
font choice affects the transcript only; the title/status/hint chrome stays Regular.

### Choosing a model

Two ways: **F1** lets you type a model name (`model` field) if you know it;
**F2** queries the server's `/api/tags` and shows a selectable list of the models
actually installed there, so you don't have to remember exact names. The chosen
model is saved and shown in the title bar.

The title bar shows the current model and a `first-last/total` line indicator.
After a reply arrives the view jumps to the **top** of that reply so you read it
from the beginning, then scroll down through it.

## How it works

- `config.rs` — host / port / model, persisted in the PDDB dict `ollama.config`.
- `net.rs` — a `POST` to ollama's `/api/chat` endpoint with `"stream": false`,
  via `ureq` over plain HTTP. The Xous `net` service transparently backs
  `std::net::TcpStream`, so no socket code is needed. The full conversation is
  sent each turn so the model keeps context.
- `ui.rs` — a `UxType::Chat` UI: GAM/IMEF own the predictive input line at the
  bottom and hand us a content canvas above it. The conversation is kept as a
  `(role, text)` transcript and word-wrapped into display `lines`; `scroll` is the
  index of the first visible line. Re-wrapping on demand (`rewrap`) is what lets
  the F4 font toggle re-flow the text. Finished prompts arrive via the `Line`
  opcode; arrow keys arrive via `rawkeys` in parallel with the IME and only scroll
  in **scroll mode** (F3). Sends run on a worker thread so the UI stays responsive;
  the worker wakes the main loop with `AppOp::ResponseReady` when a reply is ready.
- `predictor.rs` — a minimal IME predictor that returns no suggestions, so the
  `Chat` input line works without an autocomplete bar (modeled on
  `libs/chat/src/icontray.rs`).
- `main.rs` — server registration and the message loop.

## Limitations / ideas

- Plain HTTP only (fine for a LAN ollama). HTTPS would need the `libs/tls` trust
  flow, as in `apps/sidplayer/src/netfetch.rs`.
- Non-streaming: the whole reply arrives at once (a "Thinking…" status shows
  while waiting). Token streaming would read the body incrementally and parse
  newline-delimited JSON.
- Conversation history is kept only in RAM (cleared with F4 or on exit); it is
  not persisted to the PDDB.
