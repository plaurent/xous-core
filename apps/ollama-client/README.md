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
| F4         | display menu: toggle font size (Regular/Large) or clear; while a reply is pending, also "Stop waiting for the reply" |

The input line uses a **no-op predictor** (no autocomplete bar to compete with the
function keys). F4 opens a small menu to switch the transcript between the Regular
and Large glyph size (the reply re-wraps to fit) or to clear the conversation. The
font choice affects the transcript only; the title/status/hint chrome stays Regular.

### While waiting for a reply

The reply streams in as it's written. Before the first words arrive, the status
line counts the seconds (`Thinking… 12s`) and the app checks every 15 seconds that
the server still answers (`server OK`). If two checks in a row fail, it stops
waiting and says so. Once text is flowing, 90 seconds without any new data counts
as a lost connection. Either way your message is dropped from the context, so you
can simply send it again. You can also give up yourself with **F4 → Stop waiting
for the reply**.

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
- `net.rs` — a `POST` to ollama's `/api/chat` endpoint with `"stream": true`,
  via `ureq`; the reply comes back as newline-delimited JSON chunks. The Xous
  `net` service transparently backs `std::net::TcpStream`, so no socket code is
  needed. The full conversation is sent each turn so the model keeps context.
  `probe()` is a quick `GET /api/version` used to check the server is up.
- `ui.rs` — a `UxType::Chat` UI: GAM/IMEF own the predictive input line at the
  bottom and hand us a content canvas above it. The conversation is kept as a
  `(role, text)` transcript and word-wrapped into display `lines`; `scroll` is the
  index of the first visible line. Re-wrapping on demand (`rewrap`) is what lets
  the F4 font toggle re-flow the text. Finished prompts arrive via the `Line`
  opcode; arrow keys arrive via `rawkeys` in parallel with the IME and only scroll
  in **scroll mode** (F3). Sends run on a worker thread so the UI stays responsive;
  the worker streams the reply into a shared `Request` and wakes the main loop with
  `AppOp::Progress` (at most twice a second) and `AppOp::ResponseReady` at the end.
  A watchdog thread sends `AppOp::Tick` once a second for the status line and fails
  the request when the server stops answering or the stream stalls. Xous TCP has no
  keep-alives, so a silent connection can't be checked directly; separate probes
  and the stream's own data are the liveness signals.
- `predictor.rs` — a minimal IME predictor that returns no suggestions, so the
  `Chat` input line works without an autocomplete bar (modeled on
  `libs/chat/src/icontray.rs`).
- `main.rs` — server registration and the message loop.

## Limitations / ideas

- Plain HTTP only (fine for a LAN ollama). HTTPS would need the `libs/tls` trust
  flow, as in `apps/sidplayer/src/netfetch.rs`.
- A worker abandoned after a stall stays blocked until its 5-minute read timeout
  expires, then exits quietly; its late result is ignored.
- Conversation history is kept only in RAM (cleared with F4 or on exit); it is
  not persisted to the PDDB.
