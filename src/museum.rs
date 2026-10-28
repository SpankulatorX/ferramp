//! Browsing and downloading skins from the Winamp Skin Museum
//! (skins.webamp.org) through its public GraphQL API. The skins belong to
//! their authors; the user picks one and Ferramp fetches it for them.

use std::path::PathBuf;

use crate::gfx::{Bitmap, rgb};

pub const SITE: &str = "skins.webamp.org";
const API: &str = "https://api.webamp.org/graphql";
const FILES: &str = "https://r2.webampskins.org";
pub const PAGE: usize = 30;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub md5: String,
    pub filename: String,
}

impl Entry {
    /// The file name without its extension, for lists.
    pub fn name(&self) -> String {
        let f = &self.filename;
        match f.rsplit_once('.') {
            Some((stem, ext)) if ["wsz", "zip"].contains(&ext.to_lowercase().as_str()) => {
                stem.to_string()
            }
            _ => f.clone(),
        }
    }
}

/// Result of a background fetch, delivered to the event loop.
#[derive(Debug)]
pub enum Fetched {
    Page {
        query: String,
        offset: usize,
        result: Result<Vec<Entry>, String>,
    },
    Screenshot {
        md5: String,
        result: Result<Bitmap, String>,
    },
    Skin {
        name: String,
        result: Result<PathBuf, String>,
    },
}

/// One page of skins: the museum's own order when `query` is empty,
/// otherwise a full-text search. Adult skins are left out.
pub fn page(query: &str, offset: usize) -> Result<Vec<Entry>, String> {
    let body = if query.trim().is_empty() {
        serde_json::json!({
            "query": "query($first: Int!, $offset: Int!) { list: skins(sort: MUSEUM, \
                      first: $first, offset: $offset) { nodes { md5 filename nsfw } } }",
            "variables": { "first": PAGE, "offset": offset },
        })
    } else {
        serde_json::json!({
            "query": "query($q: String!, $first: Int!, $offset: Int!) { list: \
                      search_classic_skins(query: $q, first: $first, offset: $offset, \
                      nsfw: false) { md5 filename nsfw } }",
            "variables": { "q": query.trim(), "first": PAGE, "offset": offset },
        })
    };
    parse_page(&crate::net::post_json(API, &body)?)
}

fn parse_page(v: &serde_json::Value) -> Result<Vec<Entry>, String> {
    if let Some(msg) = v["errors"][0]["message"].as_str() {
        return Err(msg.to_string());
    }
    let list = &v["data"]["list"];
    let nodes = list["nodes"].as_array().or(list.as_array());
    let Some(nodes) = nodes else {
        return Err("unexpected reply from the museum".into());
    };
    Ok(nodes
        .iter()
        .filter(|n| !n["nsfw"].as_bool().unwrap_or(false))
        .filter_map(|n| {
            let md5 = n["md5"].as_str()?;
            let filename = n["filename"].as_str()?;
            is_md5(md5).then(|| Entry {
                md5: md5.to_string(),
                filename: filename.to_string(),
            })
        })
        .collect())
}

/// Museum ids end up in URLs and file names, so only accept real MD5 hex.
fn is_md5(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The museum's screenshot of the docked windows (275 x 348).
pub fn screenshot(md5: &str) -> Result<Bitmap, String> {
    if !is_md5(md5) {
        return Err("bad skin id".into());
    }
    let data = crate::net::get(&format!("{FILES}/screenshots/{md5}.png"), 2 << 20)?;
    let img = image::load_from_memory(&data)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let (w, h) = (img.width().min(275) as i32, img.height().min(348) as i32);
    let mut b = Bitmap::new(w, h, 0xFF00_0000);
    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x as u32, y as u32);
            // Shaped skins have transparent corners; show them on black.
            let a = p[3] as u32;
            let c = |v: u8| (v as u32 * a / 255) as u8;
            b.set(x, y, rgb(c(p[0]), c(p[1]), c(p[2])));
        }
    }
    Ok(b)
}

/// A safe file name for the skins folder.
fn file_name(e: &Entry) -> String {
    let stem: String = e
        .name()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " ._-()[]".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = stem.trim().trim_start_matches('.');
    let stem = if stem.is_empty() { &e.md5[..8] } else { stem };
    format!("{stem}.wsz")
}

/// Downloads a skin into the skins folder, checks that it loads, and
/// returns its path.
pub fn download(e: &Entry) -> Result<PathBuf, String> {
    if !is_md5(&e.md5) {
        return Err("bad skin id".into());
    }
    let data = crate::net::get(&format!("{FILES}/skins/{}.wsz", e.md5), 16 << 20)?;
    // Check that it loads before it gets a real name. The probe lives in the
    // user's own skins folder (not a shared temp dir), is hidden from the
    // skin list, and is created fresh so an existing file or link is never
    // written through.
    let dir = crate::config::skins_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let probe = dir.join(format!(".{}.download", e.md5));
    let _ = std::fs::remove_file(&probe);
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .map_err(|e| e.to_string())?;
        f.write_all(&data).map_err(|e| e.to_string())?;
    }
    let loaded = crate::skin::Skin::load(&probe, &crate::skin::Skin::builtin());
    let _ = std::fs::remove_file(&probe);
    if let Err(err) = loaded {
        return Err(format!("not a usable skin ({err})"));
    }
    let mut path = dir.join(file_name(e));
    if path.exists() && std::fs::read(&path).ok().as_deref() != Some(&data[..]) {
        let stem = file_name(e).trim_end_matches(".wsz").to_string();
        path = dir.join(format!("{stem}-{}.wsz", &e.md5[..8]));
    }
    std::fs::write(&path, &data).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(f: &str) -> Entry {
        Entry {
            md5: "0123456789abcdef0123456789abcdef".into(),
            filename: f.into(),
        }
    }

    #[test]
    fn parses_both_reply_shapes_and_drops_nsfw() {
        let browse = serde_json::json!({"data": {"list": {"nodes": [
            {"md5": "5e4f10275dcb1fb211d4a8b4f1bda236", "filename": "base-2.91.wsz", "nsfw": false},
            {"md5": "d6010aa35bed659bc1311820daa4b341", "filename": "x.wsz", "nsfw": true},
        ]}}});
        let r = parse_page(&browse).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name(), "base-2.91");
        let search = serde_json::json!({"data": {"list": [
            {"md5": "../../etc/passwd", "filename": "evil.wsz"},
            {"md5": "D6010AA35BED659BC1311820DAA4B341", "filename": "Bento.zip"},
        ]}});
        let r = parse_page(&search).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name(), "Bento");
        let err = serde_json::json!({"errors": [{"message": "boom"}]});
        assert_eq!(parse_page(&err).unwrap_err(), "boom");
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(
            file_name(&entry("Bento Classified.wsz")),
            "Bento Classified.wsz"
        );
        assert_eq!(file_name(&entry("../../x/y.zip")), "_.._x_y.wsz");
        assert_eq!(file_name(&entry("a\\b:c*.wsz")), "a_b_c_.wsz");
        assert_eq!(file_name(&entry("...")), "01234567.wsz");
    }

    /// Talks to the real museum; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_museum() {
        let first = page("", 0).unwrap();
        assert!(first.len() > 10);
        let found = page("bento", 0).unwrap();
        assert!(
            found
                .iter()
                .any(|e| e.filename.to_lowercase().contains("bento"))
        );
        let shot = screenshot(&first[0].md5).unwrap();
        assert_eq!((shot.w, shot.h), (275, 348));
    }
}
