# Mail

A graphical IMAP/SMTP mail client for Xous, built on the GAM UI stack.

It reuses the mail plumbing the `edlin` app was built on — the synchronous,
no-async SMTP/IMAP clients in `libs/mail`, plus the MIME / RFC-2047 /
quoted-printable parsing from `edlin`'s `cmds.rs` — but replaces edlin's
line-editor command surface with graphical screens driven by function keys
(the same convention `apps/vault` uses):

| Key | Screen   | What it does                                                       |
|-----|----------|-------------------------------------------------------------------|
| F1  | Inbox    | Lists the most recent messages (sender + subject) for selection; opening one fetches, decodes and displays its body. |
| F2  | Compose  | A To / Subject / Body form, then sends via SMTP.                  |
| F3  | Settings | IMAP and SMTP server, username, password and port forms, saved to the pddb. |
| F4  | Reply    | Pre-fills a compose form from the message currently open under F1 (To = sender, Subject = "Re: ...", original quoted below), then sends. |

The scrolling message view, status line and function-key events come from the
`chat` UI library (the same shell the [xous Signal client](../../../xous-signal-client)
uses); the forms and list pickers come from the `modals` service.

## A note on the crate name

The crate/package is called **`mailapp`**, not `mail`, because the workspace
already contains a library crate named `mail` (`libs/mail`, the SMTP/IMAP
client this app is built on) and two workspace members can't share a package
name. Everywhere it's user-visible the app is still "mail": the directory is
`apps/mail`, the GAM context name is `mail`, and the menu entry reads "Mail".

## Building

```
cargo xtask app-image mailapp
```

This regenerates the GAM app-menu tables (`services/gam/src/apps.rs` etc.)
from `apps/manifest.json`, compiles the app, and produces a flashable image
with it included. To try it in hosted mode, add `mailapp` to your
`cargo xtask run` service list.

## Account settings & security

Settings are entered under F3 and stored in the pddb dict `mail`, key
`config`, as `key=value` lines (`imap_host=...`, `imap_pass=...`, etc.) — the
same on-disk shape edlin used for its `mail` file. On real hardware the pddb
encrypts this at rest.

As with edlin, once loaded the credentials live decrypted in the app's RAM
for the process lifetime, and pre-filled password fields under F3 show a
`*****` sentinel rather than the stored password — but this is still a device
you shouldn't hand to someone else with mail configured.

TLS trust-on-first-use for the mail servers' certificates is handled
automatically by `libs/mail` (it prompts with a GAM modal showing the offered
chain the first time it sees an untrusted one, exactly like the HTTPS flow).

## Known gaps (inherited from the edlin mail code)

- Outgoing messages carry no `Date:` / `Message-ID:` header — the device
  needs an RTC-backed clock wired in to generate a compliant `Date:`; some
  spam filters may downgrade mail without them.
- Only INBOX is listed/read; there's no folder selection yet.
- Message bodies are rendered as plain text (the parser walks to a
  `text/plain` part and transfer-decodes quoted-printable / base64);
  attachments and `text/html` rendering are out of scope.
