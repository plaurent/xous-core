use xous::{MessageEnvelope};
use core::fmt::Write;
use std::fs::File;
use std::io::{Write as StdWrite, Error};
use std::path::PathBuf;
use std::io::{Read};
use std::net::{IpAddr, TcpStream, TcpListener};

const ACCEPT: &str = "Accept";
const ACCEPT_JSON: &str = "application/json";
const ACCEPT_TEXTHTML: &str = "text/html";

use ureq;

use retrobasic;

use mail::{ImapChunk, ImapClient, SmtpClient};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;



use std::collections::HashMap;
/////////////////////////// Common items to all commands
pub trait ShellCmdApi<'a> {
    // // user implemented:
    // // called to process the command with the remainder of the string attached
    // fn process(&mut self, args: String, env: &mut CommonEnv) -> Result<Option<String>, xous::Error>;
    // // returns my verb
    // fn verb(&self) -> &'static str;
    // called to process incoming messages that may have been origniated by the most recently issued command
    fn callback(&mut self, msg: &MessageEnvelope, _env: &mut CommonEnv) -> Result<Option<String>, xous::Error> {
        log::info!("received unhandled message {:?}", msg);
        Ok(None)
    }

    // created with cmd_api! macro
    // checks if the command matches the current verb in question
    fn matches(&self, verb: &str) -> bool;
}
// // the argument to this macro is the command verb
// macro_rules! cmd_api {
//     ($verb:expr) => {
//         fn verb(&self) -> &'static str {
//             stringify!($verb)
//         }
//         fn matches(&self, verb: &str) -> bool {
//             if verb == stringify!($verb) {
//                 true
//             } else {
//                 false
//             }
//         }
//     };
// }

use trng::*;
/////////////////////////// Command shell integration
#[derive(Debug)]
#[allow(dead_code)] // there's more in the envornment right now than we need for the demo
pub struct CommonEnv {
    llio: llio::Llio,
    com: com::Com,
    codec: codec::Codec,
    ticktimer: ticktimer_server::Ticktimer,
    gam: gam::Gam,
    cb_registrations: HashMap::<u32, String>,
    trng: Trng,
    xns: xous_names::XousNames,
}
impl CommonEnv {
    // pub fn register_handler(&mut self, verb: String) -> u32 {
    //     let mut key: u32;
    //     loop {
    //         key = self.trng.get_u32().unwrap();
    //         // reserve the bottom 1000 IDs for the main loop enums.
    //         if !self.cb_registrations.contains_key(&key) && (key > 1000) {
    //             break;
    //         }
    //     }
    //     self.cb_registrations.insert(key, verb);
    //     key
    // }
}

/*
    To add a new command:
        0. ensure that the command implements the ShellCmdApi (above)
        1. mod/use the new command
        2. create an entry for the command's storage in the CmdEnv structure
        3. initialize the persistant storage here
        4. add it to the "commands" array in the dispatch() routine below

    Side note: if your command doesn't require persistent storage, you could,
    technically, generate the command dynamically every time it's called.
*/

///// 1. add your module here, and pull its namespace into the local crate
//mod audio;     use audio::*;


enum EdlinMode {
    Inserting,
    Command,
    Editing
}

pub struct Edlin {
    data:Vec<std::string::String>,
    //data:Vec<String<512>>,
    mode:EdlinMode,
    last_loaded_filename:std::string::String,
    line_cursor: usize,
    current_backlight_setting: u8,
    gam: gam::Gam,
    com: com::Com,

    ///// mail (IMAP/SMTP) account settings.
    /////
    ///// Not set from source anymore: loaded from a "mail" file via the
    ///// existing "r"/"w" file commands (see apply_mail_config() and its
    ///// call sites in process() below). Typical flow: "i", type
    ///// "imap_user=..." / "imap_pass=..." / etc. one per line, "." to
    ///// leave insert mode, "w mail" to save -- or "r mail" on every
    ///// subsequent launch to reload into these fields. The defaults
    ///// below are intentionally blank/placeholder so a forgotten
    ///// "r mail" fails loudly (connection error) instead of silently
    ///// trying to talk to nothing.
    /////
    ///// The "mail" file is stored the same way any other Edlin file is
    ///// (a pddb-backed dict, encrypted at rest on real hardware), which
    ///// is a meaningfully better place for a plaintext password to live
    ///// than baked into the firmware binary -- but it's still plaintext
    ///// once decrypted into this struct's RAM for the process lifetime,
    ///// so still not something to hand your device to someone else with
    ///// mail configured.
    imap_user: std::string::String,
    imap_pass: std::string::String,
    imap_host: std::string::String,
    imap_port: u16,

    smtp_user: std::string::String,
    smtp_pass: std::string::String,
    /// Envelope/header "From" address. Some providers require this to
    /// match smtp_user (or an alias of it) or they'll reject the send.
    smtp_from: std::string::String,
    smtp_host: std::string::String,
    smtp_port: u16,
}

///// Mail helpers (parsing only, no self needed) /////

/// Blank "mail" config template loaded by "r mail" when no "mail" file
/// has been saved yet, so there's something to fill in and save rather
/// than an empty buffer. Deliberately omits imap_port/smtp_port:
/// apply_mail_config() leaves those at their built-in defaults (993/465)
/// when the key is absent, so there's no need to spell them out unless
/// non-default ports are actually wanted.
fn mail_template() -> Vec<std::string::String> {
    ["imap_user=", "imap_pass=", "imap_host=", "smtp_user=", "smtp_pass=", "smtp_from=", "smtp_host="]
        .iter()
        .map(|s| std::string::String::from(*s))
        .collect()
}

/// Parses the message count out of a SELECT response's "* <n> EXISTS"
/// untagged line.
fn parse_exists(select_response: &[std::string::String]) -> Option<u32> {
    select_response.iter().find_map(|line| {
        let mut tokens = line.split_whitespace();
        if tokens.next()? != "*" {
            return None;
        }
        let n: u32 = tokens.next()?.parse().ok()?;
        if tokens.next()?.eq_ignore_ascii_case("EXISTS") { Some(n) } else { None }
    })
}

/// Parses the sequence number out of a FETCH response's leading
/// "* <n> FETCH ..." text.
fn parse_seq_num(text: &str) -> Option<u32> {
    let mut tokens = text.split_whitespace();
    if tokens.next()? != "*" {
        return None;
    }
    tokens.next()?.parse::<u32>().ok()
}

/// Case-insensitive (ASCII-only) substring search that returns a byte
/// offset safe to slice the original `&str` at. Deliberately not
/// `haystack.to_lowercase().find(...)`: lowercasing can change a
/// string's byte length for non-ASCII input, which would make the
/// returned offset unsafe to use against the original slice.
fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    for i in 0..=(h.len() - n.len()) {
        if h[i..i + n.len()].eq_ignore_ascii_case(n) {
            return Some(i);
        }
    }
    None
}

/// Pulls the Subject: header value out of a header-fields fetch response.
///
/// Deliberately does NOT use header_value() below, and searches
/// unanchored instead: the IMAP literal carrying this header text gets
/// concatenated directly onto the tail of the preceding
/// "* n FETCH (BODY[HEADER.FIELDS (SUBJECT)] " response syntax with no
/// line break in between (literals are just inline string data on the
/// wire), so "Subject:" is essentially never the first thing on a line
/// here -- unlike a real RFC 5322 header block, where header_value()'s
/// stricter line-anchored matching is the correct (and necessary) choice
/// (see its doc comment for why unanchored matching breaks there).
///
/// Decodes RFC 2047 encoded-words (e.g. "=?UTF-8?Q?Caf=C3=A9?=" ->
/// "Café") via decode_rfc2047() below, so non-ASCII subjects show up
/// readable instead of with the raw "=?charset?Q?...?=" wrapper and its
/// "=XX" escapes passed straight through.
fn extract_subject(header_text: &str) -> std::string::String {
    if let Some(pos) = find_ascii_ci(header_text, "subject:") {
        let after = &header_text[pos + "subject:".len()..];
        let lines: Vec<&str> = after.lines().collect();
        if !lines.is_empty() {
            let mut subject = lines[0].trim().to_string();
            let mut j = 1;
            while j < lines.len() && (lines[j].starts_with(' ') || lines[j].starts_with('\t')) {
                subject.push(' ');
                subject.push_str(lines[j].trim());
                j += 1;
            }
            if !subject.is_empty() {
                return decode_rfc2047(&subject);
            }
        }
    }
    std::string::String::from("(no subject)")
}

/// Flattens a FETCH response's chunks into one lossy-UTF8 string. Fine
/// for header-fields fetches (ASCII/mostly-ASCII); for full bodies with
/// binary attachments, work with the ImapChunk::Literal bytes directly
/// instead of going through this.
fn flatten_chunks(chunks: &[ImapChunk]) -> std::string::String {
    chunks.iter().map(|c| std::string::String::from_utf8_lossy(c.as_bytes()).into_owned()).collect()
}

/// Splits raw message/part text into (header block, body) at the first
/// blank line. Body is "" if no blank-line boundary exists (malformed or
/// headers-only text).
fn split_headers_body(raw_text: &str) -> (&str, &str) {
    if let Some(pos) = raw_text.find("\r\n\r\n") {
        (&raw_text[..pos], &raw_text[pos + 4..])
    } else if let Some(pos) = raw_text.find("\n\n") {
        (&raw_text[..pos], &raw_text[pos + 2..])
    } else {
        (raw_text, "")
    }
}

/// Returns a header's value by actually parsing the header block into
/// fields -- requiring the header name to start a real field line, not
/// just appear somewhere in the block's text -- and honoring RFC 5322
/// folding (a line starting with whitespace continues the previous
/// field).
///
/// The anchoring is required, not optional: header values routinely
/// contain colon-separated text that looks like a header name.
/// DKIM-Signature's "h=" tag, for example, lists the names of every
/// header it covers -- "h=From:To:Subject:Date:Content-Type:MIME-Version:
/// References" -- as part of one long folded DKIM-Signature value. An
/// unanchored search for "content-type:" matches inside that list, and
/// the folding-continuation logic then swallows the rest of the folded
/// DKIM signature (its bh= and b= tags) as if that were the Content-Type
/// value -- which is exactly the bogus "content-type: ...bh=...; b=..."
/// this function used to produce before it was rewritten to parse fields
/// properly.
///
/// Case-insensitive on the header name; the returned value is trimmed
/// but case-preserved, so compare it lowercased if that matters to the
/// caller. Callers must pass the header block for the *specific part*
/// they care about (see split_headers_body / find_text_part below) --
/// this doesn't know anything about MIME structure on its own.
fn header_value(header_block: &str, name: &str) -> Option<std::string::String> {
    let lower_name = name.to_lowercase();
    let lines: Vec<&str> = header_block.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with(' ') || line.starts_with('\t') {
            i += 1; // orphan continuation line -- not a field start, skip
            continue;
        }
        match line.find(':') {
            Some(colon) => {
                let field_name = line[..colon].trim();
                // Walk past this field's continuation lines regardless of
                // whether it's a match, so they're never mistaken for a
                // field start on a later iteration.
                let mut j = i + 1;
                while j < lines.len() && (lines[j].starts_with(' ') || lines[j].starts_with('\t')) {
                    j += 1;
                }
                if field_name.eq_ignore_ascii_case(&lower_name) {
                    let mut value = line[colon + 1..].trim().to_string();
                    for cont in &lines[i + 1..j] {
                        value.push(' ');
                        value.push_str(cont.trim());
                    }
                    return Some(value);
                }
                i = j;
            }
            None => i += 1,
        }
    }
    None
}

/// Extracts the "boundary" parameter from a Content-Type header value,
/// e.g. `multipart/alternative; boundary="abc123"` or
/// `multipart/mixed; boundary=abc123`.
fn parse_boundary(content_type_value: &str) -> Option<std::string::String> {
    let pos = find_ascii_ci(content_type_value, "boundary=")?;
    let after = content_type_value[pos + "boundary=".len()..].trim_start();
    if let Some(rest) = after.strip_prefix('"') {
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    } else {
        let end = after.find(|c: char| c == ';' || c.is_whitespace()).unwrap_or(after.len());
        let value = after[..end].trim();
        if value.is_empty() { None } else { Some(value.to_string()) }
    }
}

/// Splits a multipart body into the raw (still headers+body combined)
/// text of each part, given the boundary string from the enclosing
/// Content-Type header. Preamble before the first boundary line and
/// epilogue after the closing "--boundary--" are discarded, the same as
/// a real mail client would do with them.
fn split_multipart(body: &str, boundary: &str) -> Vec<std::string::String> {
    let delim = format!("--{boundary}");
    let mut parts = Vec::new();
    let mut remaining = match body.find(&delim) {
        Some(pos) => &body[pos..],
        None => return parts, // doesn't actually contain the boundary
    };
    loop {
        remaining = &remaining[delim.len()..];
        if remaining.starts_with("--") {
            break; // closing delimiter "--boundary--"
        }
        remaining =
            remaining.strip_prefix("\r\n").or_else(|| remaining.strip_prefix('\n')).unwrap_or(remaining);
        match remaining.find(&delim) {
            Some(next_pos) => {
                parts.push(remaining[..next_pos].to_string());
                remaining = &remaining[next_pos..];
            }
            None => {
                parts.push(remaining.to_string());
                break;
            }
        }
    }
    parts
}

/// Walks a (possibly nested) multipart structure looking for a readable
/// text/plain part, preferring it over text/html or anything else.
/// Falls back to the first leaf part found if no text/plain part exists,
/// and returns the input unchanged if it isn't multipart at all (the
/// common case: most mail is a single part) or the boundary can't be
/// parsed out of a malformed Content-Type.
///
/// `depth` bounds the recursion: real messages are rarely more than 2-3
/// multipart levels deep (e.g. multipart/mixed > multipart/alternative >
/// multipart/related), this just stops it from ever running away on a
/// malformed or adversarial structure.
fn find_text_part(header_block: &str, body: &str, depth: u8) -> (std::string::String, std::string::String) {
    let content_type = header_value(header_block, "content-type").unwrap_or_default();
    if depth == 0 || !content_type.to_lowercase().starts_with("multipart/") {
        return (header_block.to_string(), body.to_string());
    }
    let boundary = match parse_boundary(&content_type) {
        Some(b) => b,
        None => return (header_block.to_string(), body.to_string()),
    };
    let raw_parts = split_multipart(body, &boundary);
    if raw_parts.is_empty() {
        return (header_block.to_string(), body.to_string());
    }

    let mut fallback: Option<(std::string::String, std::string::String)> = None;
    for raw_part in &raw_parts {
        let (part_headers, part_body) = split_headers_body(raw_part);
        let part_content_type = header_value(part_headers, "content-type").unwrap_or_default().to_lowercase();

        let (resolved_headers, resolved_body) = if part_content_type.starts_with("multipart/") {
            find_text_part(part_headers, part_body, depth - 1)
        } else {
            (part_headers.to_string(), part_body.to_string())
        };

        let resolved_content_type =
            header_value(&resolved_headers, "content-type").unwrap_or_default().to_lowercase();
        if resolved_content_type.starts_with("text/plain") || resolved_content_type.is_empty() {
            return (resolved_headers, resolved_body);
        }
        if fallback.is_none() {
            fallback = Some((resolved_headers, resolved_body));
        }
    }
    fallback.unwrap_or_else(|| (header_block.to_string(), body.to_string()))
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decodes RFC 2045 quoted-printable content: "=XX" is a hex-escaped
/// byte (this is where things like "=E2=80=9C" -- the UTF-8 bytes for a
/// curly left double quote -- come from), and a trailing "=" at the end
/// of a line is a soft line break (join with the next line, no newline
/// inserted). Everything else passes through unchanged.
///
/// Malformed escapes (an "=" not followed by two hex digits or a line
/// break) are left as a literal "=" rather than erroring -- a
/// best-effort readable rendering beats refusing to show a slightly
/// malformed message.
fn decode_quoted_printable(input: &str) -> std::string::String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            if bytes.get(i + 1) == Some(&b'\r') && bytes.get(i + 2) == Some(&b'\n') {
                i += 3; // soft line break
                continue;
            }
            if bytes.get(i + 1) == Some(&b'\n') {
                i += 2; // soft line break (bare LF)
                continue;
            }
            if let (Some(&h1), Some(&h2)) = (bytes.get(i + 1), bytes.get(i + 2)) {
                if let (Some(hi), Some(lo)) = (hex_digit(h1), hex_digit(h2)) {
                    out.push((hi << 4) | lo);
                    i += 3;
                    continue;
                }
            }
            out.push(b'=');
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    std::string::String::from_utf8_lossy(&out).into_owned()
}

/// Decodes RFC 2047 "Q" encoding, used inside encoded-words
/// (=?charset?Q?...?=). Same "=XX" hex-escape idea as body
/// quoted-printable (decode_quoted_printable above), reusing the same
/// hex_digit() helper, but with two differences: "_" decodes to a space
/// (a literal space can't appear inside an encoded-word, since whitespace
/// is what delimits header tokens, so encoders substitute "_" instead),
/// and there's no soft-line-break handling -- an encoded-word is a single
/// token that never spans a line.
fn decode_rfc2047_q(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'_' => {
                out.push(b' ');
                i += 1;
            }
            b'=' => {
                if let (Some(&h1), Some(&h2)) = (bytes.get(i + 1), bytes.get(i + 2)) {
                    if let (Some(hi), Some(lo)) = (hex_digit(h1), hex_digit(h2)) {
                        out.push((hi << 4) | lo);
                        i += 3;
                        continue;
                    }
                }
                out.push(b'=');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// Interprets already-decoded encoded-word bytes according to the
/// declared charset. Only UTF-8 and ISO-8859-1/US-ASCII get an exact
/// mapping -- both are simple, total byte<->codepoint relationships
/// (ISO-8859-1's code points are just its byte values, 0-255). Anything
/// else falls back to a lossy UTF-8 interpretation of the raw bytes,
/// which won't render correctly for that charset but won't corrupt
/// surrounding text or panic either. Full charset conversion (Windows-1252,
/// GB2312, Shift-JIS, etc.) is out of scope here.
fn bytes_to_string_for_charset(bytes: &[u8], charset: &str) -> std::string::String {
    let lower = charset.to_lowercase();
    if lower == "us-ascii" || lower == "iso-8859-1" || lower == "iso8859-1" || lower == "latin1" {
        bytes.iter().map(|&b| b as char).collect()
    } else {
        std::string::String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Tries to decode one RFC 2047 encoded-word at the start of `s`
/// (=?charset?encoding?encoded-text?=). Returns the decoded text and the
/// byte length of the token consumed (so the caller can skip past it),
/// or None if `s` doesn't start with a well-formed encoded-word.
fn try_decode_encoded_word(s: &str) -> Option<(std::string::String, usize)> {
    if !s.starts_with("=?") {
        return None;
    }
    let rest = &s[2..];
    let charset_end = rest.find('?')?;
    let charset = &rest[..charset_end];
    let after_charset = &rest[charset_end + 1..];
    // encoding must be exactly one (ASCII) char, immediately followed by '?'
    if after_charset.as_bytes().get(1) != Some(&b'?') {
        return None;
    }
    let encoding = *after_charset.as_bytes().first()?;
    let after_encoding = &after_charset[2..];
    let text_end = after_encoding.find("?=")?;
    let encoded_text = &after_encoding[..text_end];

    let decoded_bytes: Vec<u8> = match encoding.to_ascii_uppercase() {
        b'Q' => decode_rfc2047_q(encoded_text),
        b'B' => B64.decode(encoded_text.as_bytes()).ok()?,
        _ => return None,
    };
    let decoded_string = bytes_to_string_for_charset(&decoded_bytes, charset);

    // "=?" + charset + "?" + encoding(1) + "?" + encoded_text + "?="
    let total_len = 2 + charset_end + 1 + 1 + 1 + text_end + 2;
    Some((decoded_string, total_len))
}

/// Decodes every RFC 2047 encoded-word in a header value, e.g. a Subject
/// like "=?UTF-8?Q?Caf=C3=A9?=" or "=?UTF-8?B?SGVsbG8sIHdvcmxkIQ==?=".
/// This is where things like the literal "=?utf-8?Q?" wrapper and stray
/// "=80"-style escapes inside it (which are just Q-encoding's hex
/// escapes, same idea as body quoted-printable) come from when a subject
/// isn't decoded.
///
/// Per RFC 2047, whitespace that separates two *adjacent* encoded-words
/// is removed on decode (a long non-ASCII subject is often split into
/// several encoded-words by the sender's client); whitespace next to
/// plain text is left alone. Plain-text runs (no "=?...?=" involved) pass
/// through unchanged, including any raw non-ASCII UTF-8 a sender put in
/// the header directly instead of RFC-2047-encoding it.
fn decode_rfc2047(input: &str) -> std::string::String {
    let mut out = std::string::String::with_capacity(input.len());
    let mut chars = input.char_indices().peekable();
    let mut last_was_encoded_word = false;

    while let Some((idx, ch)) = chars.next() {
        if ch == '=' && input[idx..].starts_with("=?") {
            if let Some((decoded, token_len)) = try_decode_encoded_word(&input[idx..]) {
                out.push_str(&decoded);
                let end = idx + token_len;
                while let Some(&(next_idx, _)) = chars.peek() {
                    if next_idx >= end {
                        break;
                    }
                    chars.next();
                }
                last_was_encoded_word = true;
                continue;
            }
        }

        if ch.is_whitespace() && last_was_encoded_word {
            let ws_start = idx;
            let mut ws_end = idx + ch.len_utf8();
            while let Some(&(next_idx, next_ch)) = chars.peek() {
                if next_ch.is_whitespace() {
                    ws_end = next_idx + next_ch.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            if let Some((decoded, token_len)) = try_decode_encoded_word(&input[ws_end..]) {
                out.push_str(&decoded);
                let end = ws_end + token_len;
                while let Some(&(next_idx, _)) = chars.peek() {
                    if next_idx >= end {
                        break;
                    }
                    chars.next();
                }
                last_was_encoded_word = true;
                continue;
            }
            // not adjacent to another encoded-word -- keep the whitespace
            out.push_str(&input[ws_start..ws_end]);
            last_was_encoded_word = false;
            continue;
        }

        out.push(ch);
        last_was_encoded_word = false;
    }

    out
}

///// HTML / link cleanup helpers (ported from apps/mail) /////

/// Reduces an HTML body to readable plain text: drops <script>/<style>
/// contents and comments, turns block-level tags into line breaks and
/// inline tags into nothing, decodes the common HTML entities, then
/// collapses the resulting whitespace so it paginates cleanly on the
/// narrow LCD. This is a deliberately lightweight, best-effort converter
/// (no DOM / CSS) -- enough to make an HTML-only message readable, not a
/// full renderer.
fn strip_html(input: &str) -> std::string::String {
    // 1. Drop <script>/<style> element contents and HTML comments outright,
    //    so their internals never leak into the text.
    let without_blocks = remove_html_comments(&remove_html_element(&remove_html_element(input, "script"), "style"));

    // 2. Walk the remaining markup: copy text runs, and replace each tag
    //    with a newline (block-level tags) or nothing (inline tags).
    let mut out = std::string::String::with_capacity(without_blocks.len());
    let mut rest = without_blocks.as_str();
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let after = &rest[lt..];
        match after.find('>') {
            Some(gt) => {
                if tag_breaks_line(&after[1..gt]) {
                    out.push('\n');
                }
                rest = &after[gt + 1..];
            }
            None => {
                // Unterminated '<': treat the remainder as literal text.
                out.push_str(after);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);

    // 3. Decode entities, then normalize whitespace.
    collapse_whitespace(&decode_html_entities(&out))
}

/// Removes every `<tag ...>...</tag>` element (contents included) from
/// `input`, matching the tag name case-insensitively. An unterminated
/// element drops everything from its start to the end. Used for `<script>`
/// and `<style>`, whose bodies must never reach the reader as text.
fn remove_html_element(input: &str, tag: &str) -> std::string::String {
    let open = format!("<{tag}");
    let close = format!("</{tag}");
    let mut out = std::string::String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = find_ascii_ci(rest, &open) {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        match find_ascii_ci(after, &close) {
            Some(close_rel) => match after[close_rel..].find('>') {
                Some(gt) => rest = &after[close_rel + gt + 1..],
                None => return out, // malformed closing tag: drop the rest
            },
            None => return out, // unclosed element: drop the rest
        }
    }
    out.push_str(rest);
    out
}

/// Removes HTML comments (`<!-- ... -->`). An unterminated comment drops to
/// the end of the input.
fn remove_html_comments(input: &str) -> std::string::String {
    let mut out = std::string::String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start + 4..].find("-->") {
            Some(end_rel) => rest = &rest[start + 4 + end_rel + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Whether a tag (its inner text, without the angle brackets -- e.g. `p`,
/// `/div`, `br /`, `td colspan="2"`) is block-level, so removing it should
/// leave a line break behind. Inline tags (`a`, `span`, `b`, ...) return
/// false and simply vanish.
fn tag_breaks_line(tag: &str) -> bool {
    let name: std::string::String = tag
        .trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<std::string::String>()
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "br" | "p"
            | "div"
            | "tr"
            | "td"
            | "th"
            | "li"
            | "ul"
            | "ol"
            | "dl"
            | "dd"
            | "dt"
            | "table"
            | "thead"
            | "tbody"
            | "caption"
            | "blockquote"
            | "pre"
            | "hr"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "figure"
            | "figcaption"
            | "address"
            | "form"
            | "fieldset"
    )
}

/// Decodes the HTML entities common in mail: the named ones (`&amp;`,
/// `&lt;`, `&nbsp;`, a few typographic ones) and numeric character
/// references (`&#39;`, `&#x2019;`). Unknown or malformed entities are left
/// verbatim.
fn decode_html_entities(input: &str) -> std::string::String {
    let mut out = std::string::String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        // A real entity is short and ';'-terminated; bound the search so a
        // stray '&' in prose doesn't swallow the rest of the line.
        match after.find(';') {
            Some(semi) if semi <= 12 => match decode_html_entity(&after[..semi]) {
                Some(decoded) => {
                    out.push_str(&decoded);
                    rest = &after[semi + 1..];
                }
                None => {
                    out.push('&');
                    rest = after;
                }
            },
            _ => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decodes one entity body (the text between `&` and `;`). Returns None for
/// anything unrecognized so the caller can emit it unchanged.
fn decode_html_entity(entity: &str) -> Option<std::string::String> {
    // Numeric character reference: &#123; or &#x1F600;
    if let Some(num) = entity.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse::<u32>().ok()?,
        };
        return char::from_u32(code).map(|c| c.to_string());
    }
    let ch = match entity.to_ascii_lowercase().as_str() {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "hellip" => '\u{2026}',
        "copy" => '\u{00A9}',
        "reg" => '\u{00AE}',
        "trade" => '\u{2122}',
        "lsquo" | "rsquo" | "sbquo" => '\'',
        "ldquo" | "rdquo" | "bdquo" => '"',
        _ => return None,
    };
    Some(ch.to_string())
}

/// Collapses the whitespace left by tag removal: within each line, runs of
/// spaces/tabs become a single space and the ends are trimmed; across lines,
/// a run of blank lines is reduced to at most one, and leading/trailing
/// blank lines are dropped. This keeps stripped HTML from paginating into a
/// mostly-empty reader.
fn collapse_whitespace(input: &str) -> std::string::String {
    let mut out = std::string::String::with_capacity(input.len());
    let mut blank_run = 0usize;
    for raw in input.split('\n') {
        // Squeeze intra-line whitespace to single spaces and trim the ends.
        let mut line = std::string::String::with_capacity(raw.len());
        let mut prev_space = false;
        for ch in raw.chars() {
            if ch.is_whitespace() {
                if !prev_space {
                    line.push(' ');
                    prev_space = true;
                }
            } else {
                line.push(ch);
                prev_space = false;
            }
        }
        let line = line.trim();
        if line.is_empty() {
            // Allow at most one blank line between paragraphs.
            blank_run += 1;
            if blank_run == 1 && !out.is_empty() {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

/// Flattens Markdown-style inline links `[label](url)` to just their visible
/// `label`, dropping the bracket/paren syntax and the URL. Only a well-formed
/// link -- `]` immediately followed by `(`, with a non-empty label and url --
/// is rewritten; a lone `[...]` or `(...)` is left untouched. Nested brackets
/// aren't handled (label runs to the first `]`, url to the first `)`), which
/// is fine for the mail-generated links this targets. Reference-style links
/// (`[label][id]`) are not touched.
fn flatten_markdown_links(input: &str) -> std::string::String {
    let bytes = input.as_bytes();
    let mut out = std::string::String::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if bytes[i] == b'[' {
            // Try to parse `[label](url)` starting here.
            if let Some(close_rel) = input[i + 1..].find(']') {
                let label_end = i + 1 + close_rel; // index of ']'
                if input[label_end + 1..].starts_with('(') {
                    let url_start = label_end + 2; // just past "]("
                    if let Some(paren_rel) = input[url_start..].find(')') {
                        let label = &input[i + 1..label_end];
                        let url = &input[url_start..url_start + paren_rel];
                        if !label.is_empty() && !url.is_empty() {
                            out.push_str(label);
                            i = url_start + paren_rel + 1; // past ')'
                            continue;
                        }
                    }
                }
            }
            // Not a link: emit the '[' literally and move on.
            out.push('[');
            i += 1;
        } else {
            // Copy the text run up to the next '['.
            let rel = input[i..].find('[').unwrap_or(input.len() - i);
            out.push_str(&input[i..i + rel]);
            i += rel;
        }
    }
    out
}

/// Removes parenthesized URLs -- `(https://...)`, `(www...)`, `(mailto:...)`
/// -- from `input`, along with one immediately-preceding space so a bare
/// `label (https://...)` collapses cleanly to `label`. Only a `(...)` group
/// whose trimmed contents actually look like a URL (see [`looks_like_url`])
/// is removed; ordinary parentheticals like `(see below)` are left intact.
fn strip_parenthesized_urls(input: &str) -> std::string::String {
    let bytes = input.as_bytes();
    let mut out = std::string::String::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if bytes[i] == b'(' {
            if let Some(close_rel) = input[i + 1..].find(')') {
                let content = &input[i + 1..i + 1 + close_rel];
                if looks_like_url(content.trim()) {
                    // Drop a single preceding space so we don't leave a double
                    // space or a space before punctuation behind.
                    if out.ends_with(' ') {
                        out.pop();
                    }
                    i = i + 1 + close_rel + 1; // past the ')'
                    continue;
                }
            }
            // Not a URL: emit the '(' literally and move on.
            out.push('(');
            i += 1;
        } else {
            // Copy the text run up to the next '('.
            let rel = input[i..].find('(').unwrap_or(input.len() - i);
            out.push_str(&input[i..i + rel]);
            i += rel;
        }
    }
    out
}

/// Whether `s` (the trimmed contents of a `(...)` group) looks like a URL:
/// a single whitespace-free token beginning with a known scheme or `www.`.
/// The no-whitespace rule keeps a real parenthetical phrase (which has
/// spaces) from being mistaken for a URL.
fn looks_like_url(s: &str) -> bool {
    if s.is_empty() || s.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("ftp://")
        || lower.starts_with("www.")
}

impl Edlin {


    fn is_string_numeric(&mut self, str: &std::string::String) -> bool {
        for c in str.chars() {
            if !c.is_numeric() {
                return false;
            }
        }
        return true;
    }


    fn ls(&mut self) -> Vec<std::string::String> {
        let mut result: Vec<std::string::String> = Vec::new();
        const EDLIN_DICT: &str = "edlin";
        let mut keypath = PathBuf::new();
        keypath.push(EDLIN_DICT);

        if let Ok(dir) = std::fs::read_dir(&keypath) {
            for entry in dir {
                let path0 = entry.unwrap().path();
                let path = path0.to_str().unwrap();
                log::info!("path '{}'", path);
                if path.ends_with("_line0") {
                    log::info!("LINE0 path '{}'", path);
                    // TODO use system path separator
                    let row = format!("{}", std::string::String::from(path).replacen("edlin:", "", 1).replacen("edlin/", "", 1).replace("_line0", ""));
                    result.push(row);
                }
            }
        }
        return result;
    }


    //pub fn post_string(&mut self, url: &str, request_body: &str) -> Result<ureq::Response, ureq::Error> {
    //ureq::post(&url)
    //    .set(ACCEPT, ACCEPT_JSON)
    //    .send_string(request_body)
    //}

    pub fn post_json(&mut self, url: &str, data: &str) -> Result<ureq::Response, ureq::Error> {
    ureq::post(&url)
        .set(ACCEPT, ACCEPT_JSON)
        .send_json(ureq::json!({
            "data": data
        }))
    }

    //pub fn get_json(url: &str) -> Result<ureq::Response, ureq::Error> {
    //ureq::get(&url)
    //    .set(ACCEPT, ACCEPT_JSON)
    //    .call()
    //}

    pub fn get_texthtml(&mut self, url: &str) -> Result<ureq::Response, ureq::Error> {
    ureq::get(&url)
        .set(ACCEPT, ACCEPT_TEXTHTML)
        .call()
    }

    pub fn geturl(&mut self, url:&str) -> Option<std::string::String> {
        let response = self.get_texthtml(url);
        match response {
            Ok(response) => {
                if let Ok(body) = response.into_string() {
                    Some(body)
                } else {
                    Some("Error: could not convert response into String".to_string())
                    //None
                }
            },
            Err(ureq::Error::Status(_code, response)) => {
                /* the server returned an unexpected status
                code (such as 400, 500 etc) */
                let err_body = response.into_string().unwrap();
                Some(err_body.to_string())
                //log::info!("ERROR code {} err_body = {}", code, err_body);
                //None
            }
            Err(e) => {
                Some(e.to_string())
                //log::info!("ERROR in handle_response: {:?}", e);
                //None
            }

        }
        //return self.get_texthtml(url).unwrap().into_string().unwrap();
    }

    fn rm(&mut self, filename: std::string::String) -> Result<(), Error> {
        const EDLIN_DICT: &str = "edlin";
        let mut keypath = PathBuf::new();
        keypath.push(EDLIN_DICT);

        if let Ok(dir) = std::fs::read_dir(&keypath) {
            for entry in dir {
                let path0 = entry.unwrap().path();
                let path = path0.to_str().unwrap();
                log::info!("path '{}'", path);
                // TODO use system path separator
                let needstartwith1 = format!("edlin/{}_", filename);
                let needstartwith2 = format!("edlin:{}_", filename);
                if path.starts_with(needstartwith1.as_str()) || path.starts_with(needstartwith2.as_str()) {
                    let _ = std::fs::remove_file(&path0);
                } else {
                    //log::info!("not deleting '{}'", path);
                }
            }
        }
        Ok(())
    }

    fn load(&mut self, filename: &str) -> Result<(), Error> {
        self.data.clear();
        const EDLIN_DICT: &str = "edlin";
        let mut keypath = PathBuf::new();
        keypath.push(EDLIN_DICT);
        if std::fs::metadata(&keypath).is_ok() { // keypath exists


            self.line_cursor = 0;

            loop {
                let key = format!("{}_line{}", filename, self.line_cursor);
                let mut keypathline = keypath.clone();
                keypathline.push(key);


                if let Ok(mut file)= File::open(keypathline) {
                    let mut value = std::string::String::new();
                    file.read_to_string(&mut value)?;

                    if self.line_cursor >= self.data.len() {
                        self.line_cursor = self.data.len()
                    }
                    self.data.insert(self.line_cursor, std::string::String::from(value.as_str()));
                    self.line_cursor += 1;
                    log::info!("loaded lin '{}'", value.as_str());
                } else {
                    break;
                }
                log::info!("Loaded {} lines from files.", self.data.len());
            }



        } else {
            log::info!("dict '{}' does NOT exist.. nothing has been saved", EDLIN_DICT);
        }

        Ok(())

    }

    fn save(&mut self, filename: &str) -> Result<(), Error> {
            const EDLIN_DICT: &str = "edlin";
            let mut keypath = PathBuf::new();
            keypath.push(EDLIN_DICT);
            if std::fs::metadata(&keypath).is_ok() { // keypath exists
                // log::info!("dict '{}' exists", MTXCLI_DICT);
            } else {
                log::info!("dict '{}' does NOT exist.. creating it", EDLIN_DICT);
                std::fs::create_dir_all(&keypath)?;
            }


            for (i, line) in self.data.iter().enumerate() {
                //log::info!("writing line '{}' {} ", i, line);
                let key = format!("{}_line{}", filename, i);
                let mut keypathline = keypath.clone();
                keypathline.push(key);
                File::create(keypathline)?.write_all(line.as_bytes())?;
            }


            Ok(())
    }

    ///// Mail commands /////

    /// Parses self.data as "key=value" lines -- one setting per line,
    /// e.g. "imap_host=imap.example.com" -- and applies them to the mail
    /// account fields. Called automatically from the "r"/"w" file
    /// handlers below whenever the file being read or written is named
    /// "mail", so there's no separate config command: you edit
    /// credentials the same way you edit anything else in Edlin, then
    /// "w mail" to save (first time) or "r mail" to reload them on a
    /// later launch.
    ///
    /// Recognized keys: imap_user, imap_pass, imap_host, imap_port,
    /// smtp_user, smtp_pass, smtp_from, smtp_host, smtp_port. Blank
    /// lines and lines starting with '#' are ignored (so you can leave
    /// yourself comments). Unknown keys, lines with no '=', and
    /// unparseable *_port values are collected and reported back rather
    /// than silently dropped -- a credentials file failing partway
    /// through should be obvious, not something you discover later when
    /// "ms" mysteriously can't connect.
    fn apply_mail_config(&mut self) -> std::string::String {
        let lines = self.data.clone();
        let mut applied = 0usize;
        let mut skipped: Vec<std::string::String> = Vec::new();

        for raw_line in lines.iter() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = match line.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => {
                    skipped.push(line.to_string());
                    continue;
                }
            };
            match key {
                "imap_user" => {
                    self.imap_user = value.to_string();
                    applied += 1;
                }
                "imap_pass" => {
                    self.imap_pass = value.to_string();
                    applied += 1;
                }
                "imap_host" => {
                    self.imap_host = value.to_string();
                    applied += 1;
                }
                "imap_port" => match value.parse::<u16>() {
                    Ok(p) => {
                        self.imap_port = p;
                        applied += 1;
                    }
                    Err(_) => skipped.push(line.to_string()),
                },
                "smtp_user" => {
                    self.smtp_user = value.to_string();
                    applied += 1;
                }
                "smtp_pass" => {
                    self.smtp_pass = value.to_string();
                    applied += 1;
                }
                "smtp_from" => {
                    self.smtp_from = value.to_string();
                    applied += 1;
                }
                "smtp_host" => {
                    self.smtp_host = value.to_string();
                    applied += 1;
                }
                "smtp_port" => match value.parse::<u16>() {
                    Ok(p) => {
                        self.smtp_port = p;
                        applied += 1;
                    }
                    Err(_) => skipped.push(line.to_string()),
                },
                _ => skipped.push(line.to_string()),
            }
        }

        if skipped.is_empty() {
            format!("Applied {} mail setting(s).", applied)
        } else {
            format!("Applied {} mail setting(s), skipped {}: {}", applied, skipped.len(), skipped.join("; "))
        }
    }

    /// "ms" / "ms N" -- connects to the IMAP server and loads the N most
    /// recent messages' subjects into self.data, one per line, newest
    /// first (self.data[0] is the most recent -- matches "mr 1" below).
    /// Marks nothing as read: uses BODY.PEEK[] so listing subjects has no
    /// side effects on the mailbox.
    fn imap_list_subjects(&mut self, count: usize) -> std::string::String {
        log::info!("--> list subjects");
        let mut client = match ImapClient::connect(&self.imap_host, self.imap_port) {
            Ok(c) => c,
            Err(e) => return format!("IMAP connect failed: {}", e),
        };
        log::info!("--> connect ok");
        if let Err(e) = client.login(&self.imap_user, &self.imap_pass) {
            return format!("IMAP login failed: {}", e);
        }
        log::info!("--> login ok");
        let select_resp = match client.select("INBOX") {
            Ok(r) => r,
            Err(e) => return format!("IMAP SELECT failed: {}", e),
        };
        log::info!("--> select ok");
        let total = parse_exists(&select_resp).unwrap_or(0);
        if total == 0 {
            let _ = client.logout();
            self.data.clear();
            self.line_cursor = 0;
            return std::string::String::from("Mailbox is empty.");
        }
        log::info!("--> total ok, {}" , total);

        let n = (count.max(1) as u32).min(total);
        let start = if total > n { total - n + 1 } else { 1 };
        let range = format!("{}:{}", start, total);

        let responses = match client.fetch(&range, "BODY.PEEK[HEADER.FIELDS (SUBJECT)]") {
            Ok(r) => r,
            Err(e) => {
                let _ = client.logout();
                return format!("IMAP FETCH failed: {}", e);
            }
        };
        let _ = client.logout();

        let mut items: Vec<(u32, std::string::String)> = Vec::new();
        for chunks in responses.iter() {
            let seq = chunks
                .first()
                .and_then(|c| match c {
                    ImapChunk::Text(t) => parse_seq_num(&std::string::String::from_utf8_lossy(t)),
                    ImapChunk::Literal(_) => None,
                })
                .unwrap_or(0);
            let subject = extract_subject(&flatten_chunks(chunks));
            items.push((seq, subject));
        }
        items.sort_by(|a, b| b.0.cmp(&a.0)); // descending: most recent first

        self.data.clear();
        // Row 0 is a header, not a subject -- this pushes each subject to
        // row (recency index), so the text on row 1 is the subject of the
        // message "mr 1" would load, row 2 <-> "mr 2", etc., with no +1
        // needed to translate between what's on screen and what to fetch.
        self.data.push(std::string::String::from("Email Subjects"));
        for (_, subject) in items.iter() {
            self.data.push(subject.clone());
        }
        self.line_cursor = 0;
        format!("Loaded {} subject(s).", items.len())
    }

    /// Connects to the IMAP server, selects INBOX, and fetches the raw
    /// bytes of message # (1 = most recent, 2 = second most recent, etc.)
    /// via BODY.PEEK[] (doesn't mark \Seen). Shared by "mr" (full
    /// message) and "mz" (body only).
    ///
    /// Returns (total messages in mailbox, raw message bytes) on success,
    /// or a user-facing error string.
    fn imap_fetch_raw(&mut self, recency_index: usize) -> Result<(u32, Vec<u8>), std::string::String> {
        if recency_index == 0 {
            return Err(std::string::String::from("Message number must be 1 or greater."));
        }
        let mut client =
            ImapClient::connect(&self.imap_host, self.imap_port).map_err(|e| format!("IMAP connect failed: {}", e))?;
        client.login(&self.imap_user, &self.imap_pass).map_err(|e| format!("IMAP login failed: {}", e))?;
        let select_resp = client.select("INBOX").map_err(|e| format!("IMAP SELECT failed: {}", e))?;
        let total = parse_exists(&select_resp).unwrap_or(0);
        if total == 0 {
            let _ = client.logout();
            return Err(std::string::String::from("Mailbox is empty."));
        }
        if recency_index as u32 > total {
            let _ = client.logout();
            return Err(format!("Only {} message(s) in mailbox.", total));
        }
        let seq = total - (recency_index as u32 - 1);

        let responses = match client.fetch(&seq.to_string(), "BODY.PEEK[]") {
            Ok(r) => r,
            Err(e) => {
                let _ = client.logout();
                return Err(format!("IMAP FETCH failed: {}", e));
            }
        };
        let _ = client.logout();

        // Reassemble the raw message from the literal chunk(s); a
        // BODY.PEEK[] fetch returns the whole message as one literal.
        let mut raw = Vec::new();
        for chunks in responses.iter() {
            for chunk in chunks {
                if let ImapChunk::Literal(bytes) = chunk {
                    raw.extend_from_slice(bytes);
                }
            }
        }
        Ok((total, raw))
    }

    /// "mr #" -- connects to the IMAP server and loads message # (1 =
    /// most recent, 2 = second most recent, etc.) into self.data, one
    /// line per line of the raw message (headers included).
    fn imap_read_message(&mut self, recency_index: usize) -> std::string::String {
        match self.imap_fetch_raw(recency_index) {
            Ok((total, raw)) => {
                let text = std::string::String::from_utf8_lossy(&raw).into_owned();
                self.data.clear();
                for line in text.lines() {
                    self.data.push(line.to_string());
                }
                self.line_cursor = 0;
                format!("Loaded message {} of {} ({} lines).", recency_index, total, self.data.len())
            }
            Err(e) => e,
        }
    }

    /// "mz #" -- like "mr #", but skips the RFC 5322 headers: only the
    /// text after the first blank line is loaded into self.data.
    ///
    /// This is a plain header/body split, not a MIME parser. For a
    /// multipart or otherwise MIME-encoded message, "body" here is
    /// whatever raw bytes follow the top-level header block -- MIME
    /// boundaries, part headers, and quoted-printable/base64-encoded
    /// parts included verbatim. Picking out a single readable text/plain
    /// part from a multipart message is a bigger job (MIME parsing +
    /// transfer-decoding) and isn't done here.
    fn imap_read_body(&mut self, recency_index: usize) -> std::string::String {
        match self.imap_fetch_raw(recency_index) {
            Ok((total, raw)) => {
                let text = std::string::String::from_utf8_lossy(&raw).into_owned();
                let (top_headers, top_body) = split_headers_body(&text);
                let top_content_type =
                    header_value(top_headers, "content-type").unwrap_or_else(|| std::string::String::from("(none)"));
                // Most real-world mail today is multipart/alternative
                // (text/plain + text/html) even for plain-looking
                // messages -- walk down to the text/plain leaf part
                // rather than assuming the message is single-part.
                let (part_headers, part_body) = find_text_part(top_headers, top_body, 4);
                let part_content_type = header_value(&part_headers, "content-type")
                    .unwrap_or_else(|| std::string::String::from("(none)"));
                // Decode using *that part's own* Content-Transfer-Encoding,
                // not the top-level message's -- a multipart envelope's
                // top-level CTE is usually absent/7bit; the encoding that
                // actually applies to this text lives on the part header.
                let cte = header_value(&part_headers, "content-transfer-encoding").map(|v| v.to_lowercase());
                let cte_display = cte.clone().unwrap_or_else(|| std::string::String::from("(none)"));
                let body = match cte.as_deref() {
                    Some("quoted-printable") => decode_quoted_printable(&part_body),
                    _ => part_body,
                };

                // find_text_part prefers a text/plain alternative, but many
                // messages are HTML-only -- in which case "body" above is raw
                // HTML markup. Reduce it to readable plain text. A text/plain
                // part is left untouched (there's nothing to strip); detected
                // by the resolved part's Content-Type so we never mangle a
                // real plaintext body that merely contains angle brackets.
                let is_html = header_value(&part_headers, "content-type")
                    .map(|v| v.to_lowercase().contains("text/html"))
                    .unwrap_or(false);
                let body = if is_html { strip_html(&body) } else { body };

                // Flatten Markdown-style inline links `[label](url)` (from the
                // sender's own text/plain alternative, or left behind after
                // stripping HTML) down to just `label`.
                let body = flatten_markdown_links(&body);

                // Remove any leftover parenthesized URLs, e.g. the bare
                // `label (https://...)` link style that has no `[label]` for
                // the flatten pass to catch. Runs last so it also mops up
                // parens exposed by the flatten step.
                let body = strip_parenthesized_urls(&body);

                self.data.clear();
                for line in body.lines() {
                    self.data.push(line.to_string());
                }
                self.line_cursor = 0;
                // Diagnostic tail on the status line: what we detected at
                // the top level, which part we settled on, and what CTE
                // (if any) we decoded against. Once this is showing sane
                // values reliably it's fine to trim back down to the
                // plain "Loaded body of message..." message.
                log::info!(
                    "Loaded body of message {} of {} ({} lines). [top-type: {} | part-type: {} | part-cte: {}]",
                    recency_index,
                    total,
                    self.data.len(),
                    top_content_type,
                    part_content_type,
                    cte_display
                );
                format!(
                    "Loaded body of message {} of {} ({} lines). [top-type: {} | part-type: {} | part-cte: {}]",
                    recency_index,
                    total,
                    self.data.len(),
                    top_content_type,
                    part_content_type,
                    cte_display
                )
            }
            Err(e) => e,
        }
    }

    /// "mt user@host.name" -- connects to the SMTP server and sends
    /// self.data as a message: line 0 is the subject, the rest is the
    /// body.
    fn smtp_send(&mut self, to_addr: &str) -> std::string::String {
        if self.data.is_empty() {
            return std::string::String::from("Nothing to send: buffer is empty.");
        }
        let subject = self.data[0].clone();
        let body_text = self.data[1..].join("\r\n");

        // NOTE: no Date: or Message-ID: header -- some servers/spam
        // filters may downgrade or reject mail without them. The device
        // would need an RTC-backed clock wired in to generate a
        // compliant Date: header; left as a follow-up.
        let message = format!(
            "From: {}\r\nTo: {}\r\nSubject: {}\r\n\r\n{}",
            self.smtp_from, to_addr, subject, body_text
        );

        let mut client = match SmtpClient::connect(&self.smtp_host, self.smtp_port) {
            Ok(c) => c,
            Err(e) => return format!("SMTP connect failed: {}", e),
        };
        // EHLO wants the client's own identity, not the server's; use the
        // domain half of the From address as a reasonable stand-in since
        // this device doesn't have a real FQDN of its own.
        let ehlo_domain = self.smtp_from.split('@').nth(1).unwrap_or(self.smtp_host.as_str()).to_string();
        if let Err(e) = client.ehlo(&ehlo_domain) {
            return format!("SMTP EHLO failed: {}", e);
        }
        if let Err(e) = client.auth_login(&self.smtp_user, &self.smtp_pass) {
            return format!("SMTP auth failed: {}", e);
        }
        if let Err(e) = client.send(&self.smtp_from, &[to_addr], &message) {
            return format!("SMTP send failed: {}", e);
        }
        let _ = client.quit();
        format!("Sent to {}.", to_addr)
    }

    pub fn process(&mut self, line:&std::string::String) -> Vec<std::string::String> {

        match self.mode {
            EdlinMode::Inserting => {
                if line.trim().eq(".") {
                    self.mode = EdlinMode::Command;
                    return vec![format!(".")];
                } else {
                    if self.line_cursor >= self.data.len() {
                        self.line_cursor = self.data.len()
                    }
                    self.data.insert(self.line_cursor, std::string::String::from(line));
                    let result = format!("*{}: {}", self.line_cursor, line);
                    //let result = format!("{}", line);
                    self.line_cursor += 1;
                    return vec![result];
                }
            }
            EdlinMode::Editing => {
                self.data.remove(self.line_cursor);
                self.data.insert(self.line_cursor, std::string::String::from(line));
                self.mode = EdlinMode::Command;
                return vec![format!(".")];
            }
            EdlinMode::Command => {
                if line.len() > 0 && self.is_string_numeric(line) {
                    // is_string_numeric() accepts any Unicode numeric (and a
                    // long run of digits can overflow usize), so the parse can
                    // still fail even though every char looked numeric.
                    let idx = match line.parse::<usize>() {
                        Ok(n) => n,
                        Err(_) => return vec![format!("Invalid line number: '{}'.", line)],
                    };
                    // Selecting a line that doesn't exist (e.g. "99" in a
                    // 3-line buffer) used to panic on the get().unwrap().
                    let existing = match self.data.get(idx) {
                        Some(l) => l.clone(),
                        None => return vec![format!("No such line: {}.", idx)],
                    };
                    if existing.len() > 127 {
                        return vec![std::string::String::from("Line too long to edit. Try # wrapping.")];
                    }
                    self.mode = EdlinMode::Editing;
                    self.line_cursor = idx;
                    match self.gam.type_chars(&existing) {
                        Ok(_) => {
                            //write!(ret, "Edit the value and press enter:").unwrap()
                        }
                        _ => {
                            //write!(ret, "Couldn't type out write command.").unwrap()
                        }
                    }
                    return vec![format!("?")];
                }
                if line.starts_with("u") {
                    log::info!("--> grabbing {}", line);
                    let url = line.replace("u ", "");
                    let one_long_string = self.geturl(url.as_str()).unwrap();
                    if self.line_cursor >= self.data.len() {
                        self.line_cursor = self.data.len()
                    }
                    self.data.insert(self.line_cursor, std::string::String::from(one_long_string));
                    return vec![std::string::String::from("Grabbed URL.")];
                }
                if line.starts_with("t") {
                    log::info!("--> posting {}", line);
                    let url = line.replace("t ", "");

                    let body= self.data.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("\n");
                    // let body = std::string::String::from("This is a test of data via json");
                    let result = self.post_json(url.as_str(), body.as_str()).expect("Post didn't work");
                    log::info!("--> posted {}", line);
                    let result_string = std::string::String::from(result.into_string().unwrap());
                    log::info!("result was {}", result_string);
                    return vec![result_string];
                }
                if line.starts_with("b") {  // set brightness
                    let digits: Vec<&str> = line.matches(char::is_numeric).collect();
                    // Bare "b" (no digits) or a value over 255 won't parse as
                    // u8 -- report it instead of panicking.
                    let number = match digits.join("").parse::<u8>() {
                        Ok(n) => n,
                        Err(_) => return vec![format!("Invalid brightness: '{}'. Use e.g. b128 (0-255).", line)],
                    };
                    self.current_backlight_setting = number;
                    self.com.set_backlight(self.current_backlight_setting, self.current_backlight_setting).unwrap();
                    return vec![format!("Brightness set to {}/255.", self.current_backlight_setting)];
                }
                if line.starts_with("z") {  // run BASIC
                    let mut one_long_string = self.data.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("\n");
                    one_long_string.push_str("\n");
                    let result = retrobasic::run_prog(one_long_string);
                    return vec![format!("{}", result)];
                }

                // "ms"/"mr"/"mz"/"mt" (mail) must be checked before the
                // loose ends_with/contains checks further below
                // (v/l/n/p/i/d/#), since an argument like an email
                // address or arbitrary message count could otherwise
                // trip one of those by accident.
                let lower_line = line.to_lowercase();
                if lower_line.starts_with("ms") {
                    let arg = line.get(2..).unwrap_or("").trim();
                    let count = if arg.is_empty() { 10usize } else { arg.parse::<usize>().unwrap_or(10) };
                    return vec![self.imap_list_subjects(count)];
                }
                if lower_line.starts_with("mr") {
                    let arg = line.get(2..).unwrap_or("").trim();
                    let index = arg.parse::<usize>().unwrap_or(0);
                    return vec![self.imap_read_message(index)];
                }
                if lower_line.starts_with("mz") {
                    let arg = line.get(2..).unwrap_or("").trim();
                    let index = arg.parse::<usize>().unwrap_or(0);
                    return vec![self.imap_read_body(index)];
                }
                if lower_line.starts_with("mt") {
                    let to_addr = line.get(2..).unwrap_or("").trim().to_string();
                    if to_addr.is_empty() {
                        return vec![std::string::String::from("Please enter a recipient address after mt.")];
                    }
                    return vec![self.smtp_send(&to_addr)];
                }

                if line.ends_with("#") {
                    let mut len_for_wrap = 35;
                    if !line.starts_with("#") {
                        let digits: Vec<&str> = line.matches(char::is_numeric).collect();
                        // A non-numeric wrap prefix (e.g. "x#") collects no
                        // digits; report it rather than panic. Use a bare "#"
                        // for the default width.
                        match digits.join("").parse::<usize>() {
                            Ok(number) => len_for_wrap = number,
                            Err(_) => return vec![format!("Invalid wrap width: '{}'. Use e.g. 40# or just #.", line)],
                        }
                    }
                    let one_long_string = self.data.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ");
                    let remove_dup_spaces_and_newlines = one_long_string.replace("  ", " ").replace("\n\n", "\n");
                    let words = remove_dup_spaces_and_newlines.split(" ");
                    self.data.clear();
                    let mut line = std::string::String::new();
                    for word in words {
                        line.push_str(format!("{} ", word).as_str());
                        //log::info!("Adding line '{}' len is {} ", line, line.len());
                        if line.len() > len_for_wrap {
                            self.data.push(line.clone());
                            line = std::string::String::from("");
                        }
                    }
                    if line.len() > 0 {
                        self.data.push(line.clone());
                    }
                    return vec![format!("Wrapped to {} lines.", self.data.len())];
                }
                if line.to_lowercase().starts_with("w") {
                    let filename = line.replacen("w ", "", 1).replacen("W ", "", 1).replacen("w", "", 1).replacen("W", "", 1);
                    if filename.len() > 0 {
                        let is_mail_file = filename.eq_ignore_ascii_case("mail");

                        if let Err(_e) = self.save(&filename) {
                            return vec![std::string::String::from("Failed to save file.")];
                        }

                        if is_mail_file {
                            let summary = self.apply_mail_config();
                            return vec![format!("Save '{}' ok *{}: {}", filename, self.line_cursor, summary)];
                        }
                        return vec![format!("Save '{}' ok *{}", filename.clone(), self.line_cursor)];
                    } else {
                        if self.last_loaded_filename.len() > 0 {
                            let filename = self.last_loaded_filename.clone();
                            if let Err(_e) = self.save(&filename) {
                                return vec![std::string::String::from("Failed to save file.")];
                            }
                            return vec![format!("Save '{}' ok *{}", filename.clone(), self.line_cursor)];
                        } else {
                            return vec![std::string::String::from("Please enter a filename after w.")];
                        }
                    }
                }
                if line.to_lowercase().starts_with("r"){
                    let filename = line.replacen("r ", "", 1).replacen("R ", "", 1).replacen("r", "", 1).replacen("R", "", 1);
                    if filename.len() > 0 {
                        let is_mail_file = filename.eq_ignore_ascii_case("mail");
                        if let Err(_e) = self.load(&filename) {
                            return vec![std::string::String::from("Failed to load file.")];
                        }
                        self.last_loaded_filename = filename.clone();
                        if is_mail_file {
                            if self.data.is_empty() {
                                // load() clears self.data first and leaves
                                // it empty when there's nothing saved under
                                // this name (no "_line0" key) -- that's the
                                // only signal available, since an actually-
                                // saved-but-empty file looks identical at
                                // the storage layer. Give the user a
                                // template to fill in rather than a blank
                                // buffer and a misleading "0 settings" reply.
                                self.data = mail_template();
                                self.line_cursor = self.data.len();
                                return vec![std::string::String::from(
                                    "No saved mail file found -- loaded a blank template. Edit and \"w mail\" to save.",
                                )];
                            }
                            let summary = self.apply_mail_config();
                            return vec![format!("*{}: {}", self.line_cursor, summary)];
                        }
                        return vec![format!("*{}:", self.line_cursor)];
                    } else {
                        return vec![std::string::String::from("Please enter a filename after r.")];
                    }
                }
                if line.to_lowercase().starts_with("x"){
                    let filename = line.replacen("x ", "", 1).replacen("X ", "", 1);
                    if filename.len() > 0 {
                        if let Err(_e) = self.rm(filename) {
                            return vec![std::string::String::from("Failed to delete file.")];
                        }
                        return vec![std::string::String::from("File deleted ok.")];
                    } else {
                        return vec![std::string::String::from("Please enter a filename after x.")];
                    }
                }
                if line.to_lowercase().starts_with("?"){
                    //return vec![std::string::String::from("Edlin help.\ni insert\nd delete\nw write\nr read\n* list files\nx delete file\nnumber edit/select line\nl list all\np print\nn next n lines\n[num]# wrap text\nu get http url\nb [num] set brightness")];
                    return vec![format!("Edlin help. {}/{}.\ni insert\nd delete\nw write\nr read\n* list files\nx delete file\nnumber edit/select line\nl list all\np print\nn next n lines\n[num]# wrap text\nu get http url\nb [num] set brightness\nms [num] IMAP list num (default 10) recent subjects\nmr # IMAP load message # (1=newest)\nmz # IMAP load message # body only, no headers\nmt addr SMTP send buffer to addr (line0=subject)\nr mail / w mail  load/save IMAP+SMTP creds (key=value lines)", self.line_cursor, self.data.len())];
                }
                if line.to_lowercase().starts_with("i") || line.to_lowercase().ends_with("i") {
                    if !line.to_lowercase().starts_with("i") {
                        let digits: Vec<&str> = line.matches(char::is_numeric).collect();
                        // "<n>i" inserts before line n; a non-numeric prefix
                        // collects no digits and would panic on the parse.
                        let mut line_to_insert_before = match digits.join("").parse::<usize>() {
                            Ok(n) => n,
                            Err(_) => return vec![format!("Invalid line number: '{}'. Use e.g. 3i.", line)],
                        };
                        if line_to_insert_before >= self.data.len() {
                            line_to_insert_before = self.data.len()
                        }
                        self.line_cursor = line_to_insert_before;
                    }
                    self.mode = EdlinMode::Inserting;
                    return vec![format!("*{}:", self.line_cursor)];
                }
                if line.to_lowercase().ends_with("d") {
                    let mut del_start = self.line_cursor;
                    let mut del_cease = self.line_cursor;
                    let without_d = line.to_lowercase().replace("d", "");
                    if without_d.contains(",") {
                        let pair: Vec<&str> = without_d.split(',').collect();
                        // A malformed range like "d3," / "d,5" / "dx,y" would
                        // otherwise panic here -- report the bad input instead.
                        match (pair.get(0).and_then(|s| s.trim().parse::<usize>().ok()),
                               pair.get(1).and_then(|s| s.trim().parse::<usize>().ok())) {
                            (Some(start), Some(cease)) => {
                                del_start = start;
                                del_cease = cease;
                            }
                            _ => return vec![format!("Invalid line range: '{}'. Use e.g. d3,5.", line)],
                        }
                    } else if without_d.len() > 0 {
                        match without_d.parse::<usize>() {
                            Ok(n) => {
                                del_start = n;
                                del_cease = n;
                            }
                            Err(_) => return vec![format!("Invalid line number: '{}'. Use e.g. d3.", line)],
                        }
                    }
                    // Guard the empty buffer before computing data.len()-1,
                    // which would underflow (and panic) when there's nothing
                    // to delete.
                    if self.data.is_empty() {
                        return vec![format!("Memory is empty.")];
                    }
                    let last = self.data.len() - 1;
                    if del_cease > last {
                        del_cease = last;
                    }
                    if del_start > del_cease {
                        del_start = del_cease;
                    }
                    println!("Deleting {} to {}", del_start, del_cease);
                    // Inclusive range: "d2,5" removes lines 2,3,4 AND 5.
                    // Remove from the top down so the lower indices we still
                    // need don't shift underneath us. (The previous
                    // (start..cease).rev() was exclusive of del_cease, so a
                    // multi-line delete always dropped one line short -- the
                    // off-by-one this fixes.)
                    for i in (del_start..=del_cease).rev() {
                        self.data.remove(i);
                    }
                    if self.line_cursor > self.data.len() {
                        self.line_cursor = self.data.len();
                    }
                    return vec![format!("Deleted {} to {}", del_start, del_cease)];
                }
                if line.contains("v") || line.contains("v") {
                    return self.data.clone()
                }
                if line.contains("*") {
                    return self.ls();
                }
                if line.contains("l") || line.contains("L") {
                    let mut result: Vec<std::string::String> = Vec::new();
                    for (i, line) in self.data.iter().enumerate() {
                        if i == self.line_cursor {
                            result.insert(i, format!("*{}: {}", i, line));
                        } else {
                            result.insert(i, format!(" {}: {}", i, line));
                        }
                    }
                    return result;
                }
                if line.contains("n") || line.contains("N") {
                    if !line.to_lowercase().starts_with("n") {
                        let digits: Vec<&str> = line.matches(char::is_numeric).collect();
                        // Guard the parse: a line like "nn" collects no digits
                        // (empty string won't parse), which would otherwise panic.
                        match digits.join("").parse::<usize>() {
                            Ok(line_to_next_from) => self.line_cursor = line_to_next_from,
                            Err(_) => return vec![format!("Invalid line number: '{}'. Use e.g. n3.", line)],
                        }
                    }
                    let num_lines_per_page = 5;
                    let mut result: Vec<std::string::String> = Vec::new();
                    let mut upto = self.line_cursor + num_lines_per_page;
                    if upto > self.data.len()  {
                        upto = self.data.len();
                    }
                    for (i, line) in self.data[self.line_cursor..upto].iter().enumerate() {
                        result.insert(i, format!("{}: {}", self.line_cursor+i, line));
                    }
                    self.line_cursor = self.line_cursor + num_lines_per_page;
                    if self.line_cursor > self.data.len()-1 {
                        self.line_cursor = self.data.len()-1;
                    }
                    return result;
                }
                if line.contains("p") || line.contains("P") || line.eq("") {
                    // NOTE: Duplication of some code for "n" except no line numbers are printed
                    // and all lines are concatenated.
                    // TODO remove duplication
                    if !line.to_lowercase().starts_with("p") && !line.eq("") {
                        let digits: Vec<&str> = line.matches(char::is_numeric).collect();
                        if !digits.is_empty() {
                            // Digits present but still fallible: a very long
                            // run overflows usize. Report rather than panic.
                            match digits.join("").parse::<usize>() {
                                Ok(line_to_next_from) => self.line_cursor = line_to_next_from,
                                Err(_) => return vec![format!("Invalid line number: '{}'. Use e.g. 3p.", line)],
                            }
                        }
                    }
                    let num_lines_per_page = 5;
                    let mut result: Vec<std::string::String> = Vec::new();
                    let mut upto = self.line_cursor + num_lines_per_page;
                    if upto > self.data.len()  {
                        upto = self.data.len();
                    }
                    if self.line_cursor > self.data.len() {
                        self.line_cursor = self.data.len();
                    }
                    for (i, line) in self.data[self.line_cursor..upto].iter().enumerate() {
                        result.insert(i, format!("{}", line));
                    }

                    let one_long_string = result.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ");
                    let remove_dup_spaces = one_long_string.replace("  ", " ");

                    self.line_cursor = self.line_cursor + num_lines_per_page;
                    if self.line_cursor > self.data.len()-1 {
                        self.line_cursor = self.data.len()-1;
                    }
                    return vec!(remove_dup_spaces);
                }
            }
        }
        return Vec::new();
    }
}





pub struct CmdEnv {
    common_env: CommonEnv,
    lastverb: String,
    ///// 2. declare storage for your command here.
    //audio_cmd: Audio,
    edlin: Edlin,
}
impl CmdEnv {
    pub fn new(xns: &xous_names::XousNames) -> CmdEnv {
        let ticktimer = ticktimer_server::Ticktimer::new().expect("Couldn't connect to Ticktimer");
        log::info!("creating CommonEnv");
        let common = CommonEnv {
            llio: llio::Llio::new(&xns),
            com: com::Com::new(&xns).expect("could't connect to COM"),
            codec: codec::Codec::new(&xns).expect("couldn't connect to CODEC"),
            ticktimer,
            gam: gam::Gam::new(&xns).expect("couldn't connect to GAM"),
            cb_registrations: HashMap::new(),
            trng: Trng::new(&xns).unwrap(),
            xns: xous_names::XousNames::new().unwrap(),
        };

        let edlin = Edlin {
            data: Vec::new(),
            mode: EdlinMode::Command,
            last_loaded_filename: std::string::String::new(),
            line_cursor: 0,
            current_backlight_setting: 254,
            gam: gam::Gam::new(&xns).expect("couldn't connect to GAM"),
            com: com::Com::new(&xns).unwrap(),

            ///// Blank until "r mail" loads the mail file -- see
            ///// apply_mail_config() and its call sites in process().
            imap_user: std::string::String::new(),
            imap_pass: std::string::String::new(),
            imap_host: std::string::String::new(),
            imap_port: 993, // implicit TLS (IMAPS); see ImapClient::connect

            smtp_user: std::string::String::new(),
            smtp_pass: std::string::String::new(),
            smtp_from: std::string::String::new(),
            smtp_host: std::string::String::new(),
            smtp_port: 465, // implicit TLS (SMTPS); see SmtpClient::connect
        };
        //edlin.data.push(std::string::String::from("Hello world."));
        //edlin.data.push(std::string::String::from("This is a test."));
        //edlin.line_cursor = 2;



        log::info!("done creating CommonEnv");
        CmdEnv {
            common_env: common,
            lastverb: String::new(),
            ///// 3. initialize your storage, by calling new()
            //audio_cmd: Audio::new(&xns),
            edlin: edlin,
        }
    }

    pub fn dispatch(&mut self, maybe_cmdline: Option<&mut String>, maybe_callback: Option<&MessageEnvelope>) -> Result<Option<String>, xous::Error> {
        let mut ret = String::new();

        let commands: &mut [& mut dyn ShellCmdApi] = &mut [
            ///// 4. add your command to this array, so that it can be looked up and dispatched
            //&mut self.audio_cmd,
        ];

        if let Some(cmdline) = maybe_cmdline {

            match self.edlin.mode {
                EdlinMode::Command => {
                }
                EdlinMode::Editing => {
                }
                EdlinMode::Inserting => {
                }
            }
            let line = std::string::String::from(cmdline.as_str());
            self.edlin.com.set_backlight(self.edlin.current_backlight_setting, self.edlin.current_backlight_setting).unwrap();

            let result = self.edlin.process(&line);
            //let result = self.edlin.process(&std::string::String::from(line.trim()));

            //for result_line in result {
            for (i, result_line) in result.iter().enumerate() {  // self.data.iter().enumerate() {
                if i < result.len()-1 {
                    let _ = write!(ret, "{}\n", result_line);
                } else {
                    let _ = write!(ret, "{}", result_line);
                }
            }


            Ok(Some(ret))


            //let maybe_verb = tokenize(cmdline);

            //let mut cmd_ret: Result<Option<String::<1024>>, xous::Error> = Ok(None);
            //if let Some(verb_string) = maybe_verb {
            //    let verb = verb_string.to_str();

            //    // search through the list of commands linearly until one matches,
            //    // then run it.
            //    let mut match_found = false;
            //    for cmd in commands.iter_mut() {
            //        if cmd.matches(verb) {
            //            match_found = true;
            //            cmd_ret = cmd.process(*cmdline, &mut self.common_env);
            //            self.lastverb.clear();
            //            write!(self.lastverb, "{}", verb).expect("couldn't record last verb");
            //        };
            //    }

            //    // if none match, create a list of available commands
            //    if !match_found {
            //        let mut first = true;
            //        write!(ret, "Commands: ").unwrap();
            //        for cmd in commands.iter() {
            //            if !first {
            //                ret.append(", ")?;
            //            }
            //            ret.append(cmd.verb())?;
            //            first = false;
            //        }
            //        Ok(Some(ret))
            //    } else {
            //        cmd_ret
            //    }
            //} else {
            //    Ok(None)
            //}
        } else if let Some(callback) = maybe_callback {
            let mut cmd_ret: Result<Option<String>, xous::Error> = Ok(None);
            // first check and see if we have a callback registration; if not, just map to the last verb
            let verb = match self.common_env.cb_registrations.get(&(callback.body.id() as u32)) {
                Some(verb) => {
                    verb
                },
                None => {
                    &self.lastverb
                }
            };
            // now dispatch
            let mut verbfound = false;
            for cmd in commands.iter_mut() {
                if cmd.matches(verb) {
                    cmd_ret = cmd.callback(callback, &mut self.common_env);
                    verbfound = true;
                    break;
                };
            }
            if verbfound {
                cmd_ret
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
}

