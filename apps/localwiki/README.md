# LocalWiki

A read-only reader for wikis made with the **PaklWiki** apps on iPhone and Mac. Wikis
are downloaded over HTTP(S) from a web folder, stored locally in the PDDB, and read one
page at a time, following `[links]` between pages.

---

## Quick start

1. In PaklWiki (iPhone or Mac), open the page list, then **Export → JSON for
   Precursor**. Put the `.json` file in a folder on a web server that shows a
   directory listing.
2. Launch **LocalWiki** from the app menu, press **F1** and choose **Import wikis from
   a URL…**. Enter the folder's address (or the address of a single `.json` file).
3. The wiki opens at its Home page. Use **←/→** to pick a link and **Enter** to follow
   it, **Backspace** to go back.

---

## Controls

| Key | Action |
|---|---|
| **↑ / ↓** | Scroll a line. |
| **Space** | Scroll a page down. |
| **← / →** | Select the previous/next link. The selected link is drawn with an outline box and the page scrolls to show it. With no link selected on screen, → starts from the top of the view and ← from the bottom. |
| **Enter / centre key** | Open the selected link. |
| **Backspace** | Go back to the previous page, at the same scroll position. |
| **F1** (or **w**) | Wiki menu: open another wiki, import, or delete the open wiki. |
| **F2** (or **i**) | Index of every page in the wiki (each title is a link). |
| **F3** (or **h**) | The wiki's Home page. |
| **F4** | Cycle the text size: Small → Regular → Large. |
| **+ / –** | Larger / smaller text. |

Links are underlined. A link to a page that doesn't exist is shown as plain text with
its brackets, like `[Missing Page]`. Web addresses in a page are shown but can't be
opened.

The title bar shows the page title and, when the page is longer than the screen, which
lines are visible (`12-38/120`). The text size and the wiki you were reading are
remembered.

---

## Importing

Importing works like the SID player's downloads. Give it either:

- a **folder URL** whose page is a directory listing: every `.json` file linked
  directly on that page is downloaded (subfolders are ignored), or
- the URL of a single **`.json` file**.

Each file is checked before it's stored. Files that aren't PaklWiki exports are skipped
and listed in a summary. A wiki **replaces** any stored wiki with the same name, so
re-exporting and re-importing updates it. If the wiki you're reading is replaced, it
reopens with the new contents.

HTTPS hosts go through the usual trust-on-first-use flow: the first time, you're asked
to trust the site's root certificate. If that fails, check the device clock is set — a
clock left at the year 2000 makes every certificate look not yet valid.

A single wiki file can be up to 1 MiB.

---

## File format

The PaklWiki apps export this JSON (`version` 1); unknown fields are ignored:

```json
{
  "format": "paklwiki",
  "version": 1,
  "name": "PaklWiki",
  "home": "Home",
  "exported": "2026-10-19T17:50:00Z",
  "pages": [
    { "id": "…", "title": "Home", "modified": "2026-10-19T17:50:00Z",
      "content": "Welcome! See [Projects] and [Ideas]." }
  ]
}
```

`content` is the page text exactly as written. `[Title]` links to the page with that
title, matched exactly or, failing that, ignoring spaces around the title — the same
rule the apps use.

---

## Storage

Everything is in the PDDB's default basis:

- `localwiki.wikis` — one key per wiki (its name), holding the imported JSON.
- `localwiki.state` — the last import URL, the wiki last read, and the text size.
