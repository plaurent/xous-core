//! The wiki file format and page text.
//!
//! Wikis come from the PaklWiki iOS/macOS apps' "Export → JSON for Precursor":
//!
//! ```json
//! {
//!   "format": "paklwiki", "version": 1,
//!   "name": "PaklWiki", "home": "Home", "exported": "2026-10-19T17:50:00Z",
//!   "pages": [{ "id": "…", "title": "Home", "modified": "…", "content": "See [Ideas]" }]
//! }
//! ```
//!
//! Page text is stored exactly as written. A `[title]` is a link to the page with that
//! title, resolved the way the apps resolve it (exact title, then ignoring surrounding
//! whitespace). Unknown fields are ignored, so the exporter can add more later.

use std::collections::HashMap;

use serde::Deserialize;

const FORMAT: &str = "paklwiki";
/// Newest format version this reader understands.
const VERSION: u32 = 1;

#[derive(Deserialize)]
struct WikiFile {
    #[serde(default)]
    format: String,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    home: String,
    #[serde(default)]
    pages: Vec<Page>,
}

#[derive(Deserialize)]
pub struct Page {
    pub title: String,
    #[serde(default)]
    pub content: String,
}

pub struct Wiki {
    pub name: String,
    /// Sorted by title, ignoring case.
    pub pages: Vec<Page>,
    home: Option<usize>,
    /// Title → page. With duplicate titles, the first in `pages` wins.
    by_title: HashMap<String, usize>,
}

impl Wiki {
    /// Parse and check a wiki file. The error explains why a file was rejected.
    pub fn parse(bytes: &[u8]) -> Result<Wiki, String> {
        let file: WikiFile =
            serde_json::from_slice(bytes).map_err(|e| format!("not a wiki export ({})", e))?;
        if file.format != FORMAT {
            return Err(String::from("not a PaklWiki export"));
        }
        if file.version > VERSION {
            return Err(format!("made by a newer PaklWiki (format {}); update LocalWiki", file.version));
        }
        let name = file.name.trim().to_string();
        if name.is_empty() {
            return Err(String::from("the wiki has no name"));
        }

        let mut pages = file.pages;
        pages.sort_by_cached_key(|p| p.title.to_lowercase());

        let mut by_title = HashMap::new();
        for (i, page) in pages.iter().enumerate() {
            by_title.entry(page.title.clone()).or_insert(i);
        }
        let home = by_title.get(&file.home).or_else(|| by_title.get("Home")).copied();
        let home = home.or(if pages.is_empty() { None } else { Some(0) });

        Ok(Wiki { name, pages, home, by_title })
    }

    /// The page to open first: the exported home page, else "Home", else the first page.
    pub fn home(&self) -> Option<usize> { self.home }

    /// The page a `[link]` opens, if it exists.
    pub fn find(&self, link: &str) -> Option<usize> {
        self.by_title.get(link).or_else(|| self.by_title.get(link.trim())).copied()
    }
}

/// A piece of page text: plain text, or the text inside a `[link]`.
#[derive(Debug, PartialEq)]
pub enum Segment<'a> {
    Text(&'a str),
    Link(&'a str),
}

/// Split page text into plain text and `[links]`. A link is `[`, then one or more
/// characters that are neither `[` nor `]`, then `]` — the same rule the apps use.
pub fn segments(text: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut plain_start = 0;
    let mut i = 0;
    while let Some(rel) = text[i..].find('[') {
        let open = i + rel;
        let inner_start = open + 1;
        match text[inner_start..].find(['[', ']']) {
            Some(len) if len > 0 && text[inner_start + len..].starts_with(']') => {
                if open > plain_start {
                    out.push(Segment::Text(&text[plain_start..open]));
                }
                out.push(Segment::Link(&text[inner_start..inner_start + len]));
                plain_start = inner_start + len + 1;
                i = plain_start;
            }
            // No link starts here; a later `[` might begin one.
            _ => i = inner_start,
        }
    }
    if plain_start < text.len() {
        out.push(Segment::Text(&text[plain_start..]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_links_like_the_apps() {
        assert_eq!(
            segments("See [Projects] and [ Ideas ]."),
            vec![
                Segment::Text("See "),
                Segment::Link("Projects"),
                Segment::Text(" and "),
                Segment::Link(" Ideas "),
                Segment::Text("."),
            ]
        );
        // `[[x]]` links the inner `[x]`; empty and unclosed brackets stay text
        assert_eq!(
            segments("[[x]] [] [open"),
            vec![Segment::Text("["), Segment::Link("x"), Segment::Text("] [] [open")]
        );
        assert_eq!(segments(""), vec![]);
        assert_eq!(segments("Zażółć [jaźń]"), vec![Segment::Text("Zażółć "), Segment::Link("jaźń")]);
    }

    #[test]
    fn parses_and_resolves() {
        let json = br#"{"format":"paklwiki","version":1,"name":"Test","home":"Home","exported":"x",
            "pages":[{"title":"zeta","content":""},{"title":"Home","content":"[zeta]","id":"1"}]}"#;
        let wiki = Wiki::parse(json).unwrap();
        assert_eq!(wiki.name, "Test");
        assert_eq!(wiki.pages[0].title, "Home");
        assert_eq!(wiki.home(), Some(0));
        assert_eq!(wiki.find("zeta"), Some(1));
        assert_eq!(wiki.find(" zeta "), Some(1));
        assert_eq!(wiki.find("Zeta"), None);
    }

    #[test]
    fn rejects_other_files() {
        assert!(Wiki::parse(b"{\"name\":\"x\"}").is_err());
        assert!(Wiki::parse(b"[1,2]").is_err());
        assert!(Wiki::parse(br#"{"format":"paklwiki","version":9,"name":"x","pages":[]}"#).is_err());
        assert!(Wiki::parse(br#"{"format":"paklwiki","version":1,"name":" ","pages":[]}"#).is_err());
    }
}
