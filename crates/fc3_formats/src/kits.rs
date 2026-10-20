//! Character graphic kits (`graphics\characters\kits\*.xml`): an outfit as
//! slots of weighted body parts, each part with weighted material swaps.
//! Swaps carry tags (skin tones) so a character's head, arms and hands match.
//!
//! The kit files are plain XML; only the elements needed to dress a character
//! are read. The converter stores the parsed kits in a small text file.

use anyhow::{Context, Result};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Kit {
    pub name: String,
    /// The kit's id (the number its file is named by; entities name their
    /// kit with it), or 0.
    pub id: u64,
    pub slots: Vec<KitSlot>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct KitSlot {
    pub name: String,
    /// One of these is worn (weighted); none if empty.
    pub parts: Vec<KitPart>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct KitPart {
    /// Original `.xbg` path.
    pub model: String,
    pub weight: f32,
    pub swaps: Vec<MaterialSwap>,
    /// The tags the part gives its character (a head's `head02`).
    pub tags: Vec<String>,
}

/// Replaces one of the part's materials with a weighted pick.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MaterialSwap {
    /// Original path of the material the model uses by default.
    pub default: String,
    pub choices: Vec<MaterialChoice>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MaterialChoice {
    /// Original `.material.bin` path.
    pub material: String,
    pub weight: f32,
    pub tags: Vec<(String, TagRule)>,
}

/// How a material relates to a character tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagRule {
    /// The material gives the character this tag (or needs it, once given).
    Has,
    /// Not allowed on a character with this tag.
    Excludes,
}

impl TagRule {
    fn from_kit(t: u32) -> TagRule {
        if t == 4 {
            TagRule::Excludes
        } else {
            TagRule::Has
        }
    }

    fn code(self) -> char {
        match self {
            TagRule::Has => '+',
            TagRule::Excludes => '-',
        }
    }
}

/// Original path of a material referenced by id.
pub fn material_path(id: &str) -> String {
    format!("graphics\\_materials\\{id}.material.bin")
}

/// One XML start (or self-closing) tag or end tag.
struct Tag<'a> {
    name: &'a str,
    end: bool,
    attrs: &'a str,
}

impl<'a> Tag<'a> {
    fn attr(&self, key: &str) -> Option<&'a str> {
        let mut rest = self.attrs;
        while let Some(eq) = rest.find("=\"") {
            let k = rest[..eq].trim();
            let v_start = eq + 2;
            let v_len = rest[v_start..].find('"')?;
            if k == key {
                return Some(&rest[v_start..v_start + v_len]);
            }
            rest = &rest[v_start + v_len + 1..];
        }
        None
    }

    fn weight(&self) -> f32 {
        self.attr("weight").and_then(|w| w.parse().ok()).unwrap_or(1.0)
    }
}

fn tags(xml: &str) -> impl Iterator<Item = Tag<'_>> {
    xml.split('<').skip(1).filter_map(|chunk| {
        let body = &chunk[..chunk.find('>')?];
        if body.starts_with('?') || body.starts_with('!') {
            return None;
        }
        let end = body.starts_with('/');
        let body = body.trim_start_matches('/').trim_end_matches('/');
        let (name, attrs) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
        Some(Tag { name, end, attrs })
    })
}

/// A kit's id from its file name (`graphics\characters\Kits\<id>.xml`).
pub fn id_from_file(file: &str) -> u64 {
    let name = file.rsplit(['\\', '/']).next().unwrap_or(file);
    name.split('.').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// `(name, file)` for every kit in `graphickitlist.xml`.
pub fn parse_kit_list(xml: &str) -> Vec<(String, String)> {
    tags(xml)
        .filter(|t| !t.end && t.name == "graphickit")
        .filter_map(|t| Some((t.attr("name")?.to_string(), t.attr("file")?.to_string())))
        .collect()
}

/// Model path for a part: its descriptor's folder plus the mesh name.
fn part_model(reference: &str, mesh: &str) -> String {
    let dir = reference.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
    format!("{dir}\\{mesh}.xbg")
}

pub fn parse_kit(xml: &str) -> Result<Kit> {
    let mut kit = Kit::default();
    let mut in_swap = false;
    // (Right after a part opens: its own tags come first.)
    let mut part_opened = false;
    for t in tags(xml) {
        let own_tag = part_opened && t.name == "tag" && !t.end;
        if !t.end && t.name != "tag" {
            part_opened = t.name == "part";
        }
        match (t.name, t.end) {
            ("graphickit", false) => kit.name = t.attr("name").context("kit without a name")?.to_string(),
            ("slot", false) => kit.slots.push(KitSlot { name: t.attr("name").unwrap_or("").to_string(), parts: Vec::new() }),
            ("part", false) => {
                let (Some(slot), Some(reference), Some(mesh)) = (kit.slots.last_mut(), t.attr("ref"), t.attr("mesh")) else { continue };
                slot.parts.push(KitPart { model: part_model(reference, mesh), weight: t.weight(), swaps: Vec::new(), tags: Vec::new() });
            }
            ("overrideslot", false) => {
                let Some(part) = kit.slots.last_mut().and_then(|s| s.parts.last_mut()) else { continue };
                let Some(id) = t.attr("id") else { continue };
                part.swaps.push(MaterialSwap { default: material_path(id), choices: Vec::new() });
                in_swap = true;
            }
            ("overrideslot", true) => in_swap = false,
            ("material", false) if in_swap => {
                let Some(swap) = kit.slots.last_mut().and_then(|s| s.parts.last_mut()).and_then(|p| p.swaps.last_mut()) else { continue };
                let path = match t.attr("file") {
                    Some(f) => f.replace(".material.xml", ".material.bin").replace(".MATERIAL.XML", ".material.bin"),
                    None => material_path(t.attr("id").context("material without an id")?),
                };
                swap.choices.push(MaterialChoice { material: path, weight: t.weight(), tags: Vec::new() });
            }
            // The tags a part gives (a head's `head02`: which face rig it
            // takes); not those it excludes (type 4: the villagers' arms
            // carry `head01` so).
            ("tag", false) if own_tag && !in_swap => {
                let (Some(part), Some(name)) = (kit.slots.last_mut().and_then(|s| s.parts.last_mut()), t.attr("name")) else { continue };
                if t.attr("type").and_then(|v| v.parse::<u32>().ok()).map(TagRule::from_kit) != Some(TagRule::Excludes) {
                    part.tags.push(name.to_string());
                }
            }
            ("tag", false) if in_swap => {
                let Some(choice) = kit.slots.last_mut().and_then(|s| s.parts.last_mut()).and_then(|p| p.swaps.last_mut()).and_then(|s| s.choices.last_mut()) else { continue };
                let (Some(name), Some(rule)) = (t.attr("name"), t.attr("type").and_then(|v| v.parse::<u32>().ok())) else { continue };
                choice.tags.push((name.to_string(), TagRule::from_kit(rule)));
            }
            _ => {}
        }
    }
    // Swaps with no alternatives change nothing.
    for part in kit.slots.iter_mut().flat_map(|s| s.parts.iter_mut()) {
        part.swaps.retain(|s| !s.choices.is_empty());
    }
    Ok(kit)
}

/// The cache's form's first line.
pub const TEXT_HEADER: &str = "# kits 2";

/// Text form for the cache: `kit`, `slot`, `part`, `ptag` (a part's own
/// tag), `swap` and `mat` lines.
pub fn to_text(kits: &[Kit]) -> String {
    let mut out = format!("{TEXT_HEADER}\n");
    for k in kits {
        out += &format!("kit {}\n", k.name);
        if k.id != 0 {
            out += &format!("id {}\n", k.id);
        }
        for s in &k.slots {
            out += &format!("slot {}\n", s.name);
            for p in &s.parts {
                out += &format!("part {} {}\n", p.weight, p.model);
                for t in &p.tags {
                    out += &format!("ptag {t}\n");
                }
                for sw in &p.swaps {
                    out += &format!("swap {}\n", sw.default);
                    for c in &sw.choices {
                        let tags: Vec<String> = c.tags.iter().map(|(n, r)| format!("{}{n}", r.code())).collect();
                        out += &format!("mat {} {} {}\n", c.weight, c.material, tags.join(" ")).trim_end();
                        out.push('\n');
                    }
                }
            }
        }
    }
    out
}

pub fn from_text(text: &str) -> Vec<Kit> {
    let mut kits: Vec<Kit> = Vec::new();
    for line in text.lines() {
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "kit" => kits.push(Kit { name: rest.to_string(), id: 0, slots: Vec::new() }),
            "id" => {
                if let Some(k) = kits.last_mut() {
                    k.id = rest.parse().unwrap_or(0);
                }
            }
            "slot" => {
                if let Some(k) = kits.last_mut() {
                    k.slots.push(KitSlot { name: rest.to_string(), parts: Vec::new() });
                }
            }
            "part" => {
                let (w, model) = rest.split_once(' ').unwrap_or(("1", rest));
                if let Some(s) = kits.last_mut().and_then(|k| k.slots.last_mut()) {
                    s.parts.push(KitPart { model: model.to_string(), weight: w.parse().unwrap_or(1.0), swaps: Vec::new(), tags: Vec::new() });
                }
            }
            "ptag" => {
                if let Some(p) = kits.last_mut().and_then(|k| k.slots.last_mut()).and_then(|s| s.parts.last_mut()) {
                    p.tags.push(rest.to_string());
                }
            }
            "swap" => {
                if let Some(p) = kits.last_mut().and_then(|k| k.slots.last_mut()).and_then(|s| s.parts.last_mut()) {
                    p.swaps.push(MaterialSwap { default: rest.to_string(), choices: Vec::new() });
                }
            }
            "mat" => {
                let mut it = rest.split(' ');
                let weight = it.next().and_then(|w| w.parse().ok()).unwrap_or(1.0);
                let Some(material) = it.next() else { continue };
                let tags = it
                    .filter_map(|t| match t.split_at_checked(1) {
                        Some(("+", n)) => Some((n.to_string(), TagRule::Has)),
                        Some(("-", n)) => Some((n.to_string(), TagRule::Excludes)),
                        _ => None,
                    })
                    .collect();
                if let Some(sw) = kits.last_mut().and_then(|k| k.slots.last_mut()).and_then(|s| s.parts.last_mut()).and_then(|p| p.swaps.last_mut()) {
                    sw.choices.push(MaterialChoice { material: material.to_string(), weight, tags });
                }
            }
            _ => {}
        }
    }
    kits
}

/// Tags a character has picked up so far, and whether a material fits them.
#[derive(Default, Clone, Debug)]
pub struct Tags(pub Vec<String>);

impl Tags {
    /// Indices of the swap's choices that fit. Once the character has any tag
    /// the swap mentions, only materials sharing one of its tags fit.
    pub fn allowed(&self, swap: &MaterialSwap) -> Vec<usize> {
        let decided = swap.choices.iter().flat_map(|c| &c.tags).any(|(n, _)| self.0.contains(n));
        swap.choices
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                let excluded = c.tags.iter().any(|(n, r)| *r == TagRule::Excludes && self.0.contains(n));
                let mut has = c.tags.iter().filter(|(_, r)| *r == TagRule::Has).peekable();
                let fits = !decided || has.peek().is_none() || has.any(|(n, _)| self.0.contains(n));
                !excluded && fits
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn take(&mut self, c: &MaterialChoice) {
        for (n, r) in &c.tags {
            if *r == TagRule::Has && !self.0.contains(n) {
                self.0.push(n.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIT: &str = r#"<graphickit id="1" name="Test_Kit" unique="0" kittobone="1">
	<slot name="Head" position="0" bonename="Head">
		<partdatabase name="" weight="0">
			<part name="Head01" ref="graphics\chars\Head01.xml" mesh="Head01" weight="1" version="1">
				<tag name="head02" type="3" />
				<tag name="head01" type="4" />
				<materialdatabase name="" weight="0" />
				<defaultmaterials>
					<overrideslot name="Face" id="a-m-1">
						<materialdatabase name="" weight="0">
							<material id="b-M-2" name="Dark" weight="1" file="graphics\_materials\b-M-2.material.xml">
								<tag name="dark" type="0" />
								<tag name="pale" type="4" />
							</material>
							<material id="c-M-3" name="Pale" weight="2" file="graphics\_materials\c-M-3.material.xml">
								<tag name="pale" type="0" />
								<tag name="dark" type="4" />
							</material>
						</materialdatabase>
					</overrideslot>
					<overrideslot name="Unused" id="d-m-4">
						<materialdatabase name="" weight="0" />
					</overrideslot>
				</defaultmaterials>
			</part>
		</partdatabase>
	</slot>
	<slot name="Empty" position="1" bonename="Hips">
		<partdatabase name="" weight="0" />
	</slot>
</graphickit>"#;

    #[test]
    fn parses_and_round_trips() {
        let k = parse_kit(KIT).unwrap();
        assert_eq!(k.name, "Test_Kit");
        assert_eq!(k.slots.len(), 2);
        let p = &k.slots[0].parts[0];
        assert_eq!(p.model, "graphics\\chars\\Head01.xbg");
        assert_eq!(p.swaps.len(), 1, "empty swaps are dropped");
        assert_eq!(p.swaps[0].default, "graphics\\_materials\\a-m-1.material.bin");
        assert_eq!(p.swaps[0].choices[1].material, "graphics\\_materials\\c-M-3.material.bin");
        assert_eq!(p.swaps[0].choices[1].weight, 2.0);
        assert_eq!(p.swaps[0].choices[0].tags, vec![("dark".to_string(), TagRule::Has), ("pale".to_string(), TagRule::Excludes)]);
        // The part's own tag (which face rig the head takes), not the materials'.
        assert_eq!(p.tags, vec!["head02".to_string()]);
        assert!(k.slots[1].parts.is_empty());
        let back = from_text(&to_text(&[k.clone()]));
        assert_eq!(back, vec![k]);
    }

    #[test]
    fn tags_keep_tones_consistent() {
        let k = parse_kit(KIT).unwrap();
        let swap = &k.slots[0].parts[0].swaps[0];
        let mut t = Tags::default();
        assert_eq!(t.allowed(swap), vec![0, 1]);
        t.take(&swap.choices[0]);
        assert_eq!(t.allowed(swap), vec![0]);
        // Unrelated tags leave a swap undecided.
        let mut other = Tags(vec!["red".into()]);
        assert_eq!(other.allowed(swap), vec![0, 1]);
        other.take(&swap.choices[1]);
        assert_eq!(other.allowed(swap), vec![1]);
    }

    #[test]
    fn reads_the_kit_list() {
        let list = r#"<graphickits>
	<graphickit file="graphics\characters\Kits\1.xml" name="A" />
	<graphickit file="graphics\characters\Kits\2.xml" name="B" />
</graphickits>"#;
        assert_eq!(parse_kit_list(list), vec![("A".into(), "graphics\\characters\\Kits\\1.xml".into()), ("B".into(), "graphics\\characters\\Kits\\2.xml".into())]);
    }
}
