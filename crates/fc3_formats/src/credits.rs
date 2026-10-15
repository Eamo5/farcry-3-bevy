//! The end credits (`credits.xml`, a string table): their entries in order,
//! each a kind the credits movie knows (`AddCreditItem`'s type, numbered as
//! its `FC3CreditsConstants` number them) and its text (an image's is the
//! movie's symbol for it). Kept in the cache as a line an entry, the kind's
//! number and the text, tab-separated.

use anyhow::{Context, Result};

/// The table in the game's files.
pub const SOURCE: &str = "credits.xml";
/// The entries in the cache.
pub const CACHE_REL: &str = "ui/credits.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Name,
    Title,
    JobTitle,
    Image,
    SubTitle,
}

impl Kind {
    /// The number the movie knows it by.
    pub fn code(self) -> u8 {
        match self {
            Kind::Name => 0,
            Kind::Title => 1,
            Kind::JobTitle => 2,
            Kind::Image => 3,
            Kind::SubTitle => 4,
        }
    }

    fn from_code(code: u8) -> Option<Kind> {
        [Kind::Name, Kind::Title, Kind::JobTitle, Kind::Image, Kind::SubTitle].into_iter().find(|k| k.code() == code)
    }

    /// The kind an entry's name gives (`Credit_JobTitle_4`).
    fn from_entry(name: &str) -> Option<Kind> {
        let kind = name.strip_prefix("Credit_")?.rsplit_once('_')?.0;
        Some(match kind {
            "Name" => Kind::Name,
            "Title" => Kind::Title,
            "JobTitle" => Kind::JobTitle,
            "Image" => Kind::Image,
            "SubTitle" => Kind::SubTitle,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    pub text: String,
}

/// The table's entries in order (`<string enum="Credit_Name_1" value="..."/>`),
/// those of a kind the movie doesn't know left out.
pub fn parse(bytes: &[u8]) -> Result<Vec<Entry>> {
    let text = std::str::from_utf8(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)).context("credits table is not UTF-8")?;
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("<string") {
        rest = &rest[at + "<string".len()..];
        let end = rest.find('>').context("credits entry not closed")?;
        let tag = &rest[..end];
        rest = &rest[end..];
        let (Some(name), Some(value)) = (attribute(tag, "enum"), attribute(tag, "value")) else { continue };
        if let Some(kind) = Kind::from_entry(&name) {
            out.push(Entry { kind, text: value });
        }
    }
    Ok(out)
}

/// An attribute's value in a tag, its entities resolved.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let mut rest = tag;
    loop {
        let at = rest.find(name)?;
        let after = &rest[at + name.len()..];
        let before_ok = at == 0 || rest[..at].ends_with(char::is_whitespace);
        if let (true, Some(after)) = (before_ok, after.trim_start().strip_prefix('=')) {
            let after = after.trim_start();
            let quote = after.chars().next()?;
            let body = &after[1..];
            let close = body.find(quote)?;
            return Some(unescape(&body[..close]));
        }
        rest = after;
    }
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// The cache's form: a line an entry, `code<TAB>text`.
pub fn to_text(entries: &[Entry]) -> String {
    entries.iter().map(|e| format!("{}\t{}\n", e.kind.code(), e.text.replace(['\t', '\n', '\r'], " "))).collect()
}

pub fn from_text(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|line| {
            let (code, text) = line.split_once('\t')?;
            Some(Entry { kind: Kind::from_code(code.parse().ok()?)?, text: text.to_string() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_come_in_order_with_their_kinds_and_back_from_the_cache() {
        let xml = "\u{feff}<stringtable language=\"english\">\n<section name=\"credits\">\n\
            <string enum=\"Credit_Image_0\" value=\"Logo\" />\n\
            <string enum=\"Credit_Title_1\" value=\"A &amp; B\" />\n\
            <string enum=\"Credit_JobTitle_2\" value=\"Lead\" />\n\
            <string enum=\"Credit_Name_3\" value=\"Someone\" />\n\
            <string enum=\"Credit_SubTitle_4\" value=\"Part\" />\n\
            <string enum=\"Other_5\" value=\"left out\" />\n\
            </section></stringtable>";
        let entries = parse(xml.as_bytes()).unwrap();
        let kinds: Vec<u8> = entries.iter().map(|e| e.kind.code()).collect();
        assert_eq!(kinds, [3, 1, 2, 0, 4]);
        assert_eq!(entries[1].text, "A & B");
        assert_eq!(from_text(&to_text(&entries)), entries);
    }
}
