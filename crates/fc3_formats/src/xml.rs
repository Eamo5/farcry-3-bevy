//! A small XML reader for the game's plain XML files (sequences, kit
//! lists, subtitles): elements with attributes, nested; text content and
//! comments are skipped.

/// An element.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn f32(&self, name: &str) -> Option<f32> {
        self.attr(name).and_then(|v| v.trim().parse().ok())
    }

    pub fn u64(&self, name: &str) -> Option<u64> {
        self.attr(name).and_then(|v| v.trim().parse().ok())
    }

    pub fn u32(&self, name: &str) -> Option<u32> {
        self.attr(name).and_then(|v| v.trim().parse().ok())
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        self.attr(name).map(|v| v.trim() == "1" || v.trim().eq_ignore_ascii_case("true"))
    }

    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Every element below this one, depth first.
    pub fn walk<'a>(&'a self, f: &mut dyn FnMut(&'a Element)) {
        for c in &self.children {
            f(c);
            c.walk(f);
        }
    }
}

fn unescape(v: &str) -> String {
    if !v.contains('&') {
        return v.to_string();
    }
    v.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i] != b'=' && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        let name = &s[start..i];
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'=') {
            i += 1;
        }
        if i >= b.len() || (b[i] != b'"' && b[i] != b'\'') {
            break;
        }
        let quote = b[i];
        i += 1;
        let vstart = i;
        while i < b.len() && b[i] != quote {
            i += 1;
        }
        out.push((name.to_string(), unescape(&s[vstart..i.min(b.len())])));
        i += 1;
    }
    out
}

/// The document's root element.
pub fn parse(text: &str) -> Option<Element> {
    let mut stack: Vec<Element> = vec![Element::default()];
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        rest = &rest[open + 1..];
        if let Some(r) = rest.strip_prefix("!--") {
            rest = r.find("-->").map(|e| &r[e + 3..]).unwrap_or("");
            continue;
        }
        let Some(close) = rest.find('>') else { break };
        let tag = &rest[..close];
        rest = &rest[close + 1..];
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            let name = name.trim();
            // Close up to the matching element.
            while stack.len() > 1 {
                let done = stack.pop().unwrap();
                let matched = done.name == name;
                stack.last_mut().unwrap().children.push(done);
                if matched {
                    break;
                }
            }
            continue;
        }
        let self_closing = tag.ends_with('/');
        let body = tag.trim_end_matches('/');
        let (name, attrs) = body.split_once(|c: char| c.is_ascii_whitespace()).unwrap_or((body, ""));
        let el = Element { name: name.to_string(), attrs: parse_attrs(attrs), children: Vec::new() };
        if self_closing {
            stack.last_mut().unwrap().children.push(el);
        } else {
            stack.push(el);
        }
    }
    while stack.len() > 1 {
        let done = stack.pop().unwrap();
        stack.last_mut().unwrap().children.push(done);
    }
    stack.pop().and_then(|root| root.children.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_elements() {
        let x = r#"<?xml version="1.0"?><!-- c --><A n="1"><B v='x &amp; y'/><C><D/></C></A>"#;
        let a = parse(x).unwrap();
        assert_eq!(a.name, "A");
        assert_eq!(a.u32("n"), Some(1));
        assert_eq!(a.child("B").and_then(|b| b.attr("v")), Some("x & y"));
        assert_eq!(a.child("C").unwrap().children[0].name, "D");
    }
}
