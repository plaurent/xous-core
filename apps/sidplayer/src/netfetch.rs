//! Fetch `.sid` files from an HTTP(S) directory-index page.
//!
//! Uses `ureq` with the Xous trust-store TLS connector, so the first HTTPS
//! connection to a new host prompts the user (via `modals`) to trust its CA; the
//! decision is then remembered in the PDDB. Only files linked directly on the
//! given page are considered — subdirectories are deliberately ignored.

use std::io::Read;
use std::sync::Arc;

use tls::xtls::TlsConnector;
use ureq::Agent;
use url::Url;

/// Cap on a single downloaded file (SID tunes are a few KiB; this only guards
/// against a mislinked huge file exhausting RAM).
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// A `.sid` file discovered on the index page.
pub struct SidLink {
    /// Decoded filename, used as the display name and PDDB key, e.g. `Commando.sid`.
    pub filename: String,
    /// Absolute URL to download it from.
    pub url: String,
}

pub fn make_agent() -> Agent { ureq::builder().tls_connector(Arc::new(TlsConnector {})).build() }

/// Ensure the host's TLS certificate is trusted before we try to fetch over HTTPS.
///
/// The Precursor ships with no root CAs, and the `ureq` TLS connector's own
/// auto-trust path only *probes* the chain (it never prompts or stores trust), so
/// a first-time HTTPS host would otherwise always fail with the connector's "526 /
/// untrusted certificate chain" sentinel and no prompt. Here we run the real trust
/// flow: probe the host, and — if nothing offered is already trusted — pop the
/// "trust this certificate?" modal so the user can save a CA to the PDDB. Once
/// trusted, the subsequent `ureq` handshake finds it in the root store.
///
/// Returns Ok only for a non-HTTPS URL (no cert needed) or once at least one
/// offered certificate is trusted.
pub fn ensure_trusted(index_url: &str) -> Result<(), String> {
    let u = Url::parse(index_url).map_err(|e| format!("bad URL: {}", e))?;
    if u.scheme() != "https" {
        return Ok(());
    }
    let host = u.host_str().ok_or_else(|| "URL has no host".to_string())?;
    let tls = tls::Tls::new();
    if tls.accessible(host, true) {
        Ok(())
    } else {
        // accessible() also fails if the cert looks "not valid yet" — which on this
        // device almost always means the clock is unset (defaults to year 2000), so
        // call that out explicitly since it's the most common cause.
        Err(format!(
            "Could not establish a trusted TLS connection to {}.\n\nCommon causes:\n\
             • The device clock is not set (Precursor defaults to year 2000, which \
             makes valid certificates look \"not valid yet\"). Set the time via NTP or \
             manually, then retry.\n\
             • You declined the certificate, or the host could not be reached.\n\n\
             When the certificate list is offered, trust the root CA.",
            host
        ))
    }
}

/// GET the index page at `index_url` and return the `.sid` files linked directly
/// on it (non-recursive), de-duplicated and sorted by filename.
pub fn list_sid_files(agent: &Agent, index_url: &str) -> Result<Vec<SidLink>, String> {
    let base = Url::parse(index_url).map_err(|e| format!("bad URL: {}", e))?;
    let resp = match agent.get(base.as_str()).call() {
        Ok(r) => r,
        // 526 is the tls connector's sentinel for an untrusted certificate chain
        // (its synthetic response reads "https://example.com/ status code 526").
        Err(ureq::Error::Status(526, _)) => {
            return Err("TLS certificate for this host is not trusted. Trust the site's root \
                 CA when prompted, then try again."
                .to_string());
        }
        Err(e) => return Err(format!("fetch failed: {}", e)),
    };
    let body = resp.into_string().map_err(|e| format!("read failed: {}", e))?;

    // Directory that the index lives in, e.g. ".../tunes/" — a candidate is only
    // accepted if it resolves into exactly this directory (no subfolders).
    let base_dir = dir_path(base.path());

    let mut out: Vec<SidLink> = Vec::new();
    for href in extract_hrefs(&body) {
        let abs = match base.join(&href) {
            Ok(u) => u,
            Err(_) => continue,
        };
        if abs.host_str() != base.host_str() || abs.scheme() != base.scheme() {
            continue; // don't follow off-site links
        }
        let path = abs.path();
        if !path.to_ascii_lowercase().ends_with(".sid") {
            continue;
        }
        if dir_path(path) != base_dir {
            continue; // a subdirectory or parent — single-directory only
        }
        let filename = match path.rsplit('/').next() {
            Some(seg) if !seg.is_empty() => decode(seg),
            _ => continue,
        };
        if out.iter().any(|l| l.filename == filename) {
            continue;
        }
        // Strip any query/fragment; download the bare file URL.
        let mut dl = abs.clone();
        dl.set_query(None);
        dl.set_fragment(None);
        out.push(SidLink { filename, url: dl.to_string() });
    }
    out.sort_by(|a, b| a.filename.to_lowercase().cmp(&b.filename.to_lowercase()));
    Ok(out)
}

/// Download one file's bytes (bounded by [`MAX_FILE_BYTES`]).
pub fn download(agent: &Agent, url: &str) -> Result<Vec<u8>, String> {
    let resp = agent.get(url).call().map_err(|e| format!("{}", e))?;
    let mut rdr = resp.into_reader().take(MAX_FILE_BYTES + 1);
    let mut buf = Vec::new();
    rdr.read_to_end(&mut buf).map_err(|e| format!("{}", e))?;
    if buf.len() as u64 > MAX_FILE_BYTES {
        return Err("file too large".into());
    }
    Ok(buf)
}

/// The directory portion of a path: everything up to and including the last '/'.
fn dir_path(path: &str) -> &str { &path[..path.rfind('/').map(|i| i + 1).unwrap_or(0)] }

/// Percent-decode a single path segment for display/key use, best-effort.
fn decode(seg: &str) -> String {
    match percent_decode(seg) {
        Ok(s) => s,
        Err(_) => seg.to_string(),
    }
}

fn percent_decode(seg: &str) -> Result<String, ()> {
    let bytes = seg.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16).ok_or(())?;
            let lo = (bytes[i + 2] as char).to_digit(16).ok_or(())?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

/// Pull the target of every `href=...` attribute out of an HTML blob. Handles
/// single-, double-, and unquoted values. Deliberately tiny — directory-index
/// pages are simple generated HTML, not arbitrary markup.
fn extract_hrefs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut search = 0;
    while let Some(rel) = lower[search..].find("href") {
        let mut i = search + rel + 4;
        // skip whitespace then '='
        while i < html.len() && html.as_bytes()[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= html.len() || html.as_bytes()[i] != b'=' {
            search = search + rel + 4;
            continue;
        }
        i += 1;
        while i < html.len() && html.as_bytes()[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= html.len() {
            break;
        }
        let (start, end) = match html.as_bytes()[i] {
            q @ (b'"' | b'\'') => {
                let s = i + 1;
                match html[s..].find(q as char) {
                    Some(e) => (s, s + e),
                    None => break,
                }
            }
            _ => {
                let s = i;
                let e = html[s..]
                    .find(|c: char| c.is_ascii_whitespace() || c == '>')
                    .map(|e| s + e)
                    .unwrap_or(html.len());
                (s, e)
            }
        };
        out.push(html[start..end].to_string());
        search = end;
    }
    out
}
