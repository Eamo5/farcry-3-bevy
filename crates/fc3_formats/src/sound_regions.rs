//! The ambience's sound regions: where the world paints them, and what each
//! plays.
//!
//! Each world sector has a map of its sound regions (`sector<id>.srl`, with
//! its other generated files): 32×32 cells of 2 m, row by row from the
//! sector's south-west corner (x east, then y north), each the index
//! (`disUniqueId`) of one of the 64 regions in `soundregions_default.obj`.
//! A region has a looping bed of day and night tracks, wind and rain loops,
//! a reverb preset and calls (birds, insects, animals) in four bands: by day
//! or by night, from further off or near. The template's time curve says how
//! much each band calls at each hour.
//!
//! The sectors' logic zones (`zonesector<id>.zsr`: 64×64 cells of 1 m, laid
//! out the same, [`NONE`] where none is painted) are kept the same way: the
//! fire manager burns the ground with the profile of its zone.

use anyhow::{bail, Context, Result};

use crate::fcb::Object;
use crate::terrain::{SectorCoord, SECTOR_SIZE, WORLD_SECTORS};

pub const MAP_REL: &str = "world/sound_regions.bin";
pub const TABLE_REL: &str = "world/sound_regions.txt";
/// The logic zones' map.
pub const ZONES_REL: &str = "world/logic_zones.bin";
const MAP_MAGIC: &[u8; 8] = b"FC3SRM02";
const TABLE_MAGIC: &str = "fc3-sound-regions 1";
/// A sector's cells a side: in its sound region map, in its logic zone map.
pub const CELLS: usize = 32;
pub const ZONE_CELLS: usize = 64;
/// No region painted (a sector with no map).
pub const NONE: u8 = 0xFF;

/// The calls' bands, as the regions name them.
pub const BANDS: [&str; 4] = ["DayMediumPerspective", "DayNearPerspective", "NightMediumPerspective", "NightNearPerspective"];

/// A sector's map in the world's files.
pub fn srl_path(world: &str, c: SectorCoord) -> String {
    format!("worlds\\{world}\\levels\\{}\\generated\\worldsectors\\sector{}.srl", c.level(), c.id())
}

/// A sector's logic zones in the world's files.
pub fn zsr_path(world: &str, c: SectorCoord) -> String {
    format!("worlds\\{world}\\levels\\{}\\generated\\worldsectors\\zonesector{}.zsr", c.level(), c.id())
}

/// The world's painted regions (or zones): each sector one over all of it,
/// or its cells (`side` a side).
#[derive(Debug, Clone, PartialEq)]
pub struct RegionMap {
    side: usize,
    /// Per sector (`SectorCoord::id`): a region (`< 0x100`; [`NONE`]: no
    /// map), else `0x100 +` the index of its cells in `cells`.
    sectors: Vec<u16>,
    cells: Vec<u8>,
}

impl RegionMap {
    pub fn new(side: usize) -> RegionMap {
        RegionMap { side, sectors: vec![NONE as u16; (WORLD_SECTORS * WORLD_SECTORS) as usize], cells: Vec::new() }
    }

    /// The map from the sectors' files (`.srl`, `.zsr`), each `side`² cells.
    pub fn from_sectors(side: usize, sectors: impl IntoIterator<Item = (SectorCoord, Vec<u8>)>) -> RegionMap {
        let mut map = RegionMap::new(side);
        let area = side * side;
        for (c, cells) in sectors {
            if !c.valid() || cells.len() != area {
                continue;
            }
            let entry = if cells.iter().all(|&v| v == cells[0]) {
                cells[0] as u16
            } else {
                map.cells.extend_from_slice(&cells);
                0x100 + (map.cells.len() / area - 1) as u16
            };
            map.sectors[c.id() as usize] = entry;
        }
        map
    }

    /// The region painted at a Dunia-space (x, y) position.
    pub fn region_at(&self, x: f32, y: f32) -> Option<u8> {
        let c = SectorCoord::containing(x, y);
        if !c.valid() {
            return None;
        }
        let entry = self.sectors[c.id() as usize];
        let v = if entry < 0x100 {
            entry as u8
        } else {
            let [ox, oy] = c.origin();
            let n = self.side;
            let cell = |v: f32| ((v / (SECTOR_SIZE / n as f32)) as usize).min(n - 1);
            let (cx, cy) = (cell(x - ox), cell(y - oy));
            *self.cells.get((entry as usize - 0x100) * n * n + cy * n + cx)?
        };
        (v != NONE).then_some(v)
    }

    /// Sectors painted, and those with more than one region.
    pub fn counts(&self) -> (usize, usize) {
        (self.sectors.iter().filter(|&&e| e != NONE as u16).count(), self.cells.len() / (self.side * self.side))
    }

    /// `FC3SRM02`, then `u32` side, sector count and cell blocks, each
    /// sector's `u16` entry and the blocks.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = MAP_MAGIC.to_vec();
        out.extend_from_slice(&(self.side as u32).to_le_bytes());
        out.extend_from_slice(&(self.sectors.len() as u32).to_le_bytes());
        out.extend_from_slice(&((self.cells.len() / (self.side * self.side)) as u32).to_le_bytes());
        for e in &self.sectors {
            out.extend_from_slice(&e.to_le_bytes());
        }
        out.extend_from_slice(&self.cells);
        out
    }

    pub fn from_bytes(b: &[u8]) -> Result<RegionMap> {
        if b.len() < 20 || &b[..8] != MAP_MAGIC {
            bail!("not a region map");
        }
        let u32_at = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        let (side, sectors, blocks) = (u32_at(8), u32_at(12), u32_at(16));
        let at = 20 + sectors * 2;
        if side == 0 || side > 256 || sectors != (WORLD_SECTORS * WORLD_SECTORS) as usize || b.len() != at + blocks * side * side {
            bail!("region map of the wrong size");
        }
        let entries: Vec<u16> = b[20..at].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        if entries.iter().any(|&e| e >= 0x100 && (e - 0x100) as usize >= blocks) {
            bail!("region map's sector past its cells");
        }
        Ok(RegionMap { side, sectors: entries, cells: b[at..].to_vec() })
    }
}

/// A band of a region's calls: its events (one picked each time), how many
/// a minute, at most how many at once, and how far from the listener (m).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Calls {
    pub events: Vec<u32>,
    pub per_minute: f32,
    pub polyphony: u32,
    pub radius: (f32, f32),
}

/// A region's ambience: its bed (the first `day_tracks` of its tracks play
/// by day, the next `night_tracks` by night) and its level (dB), wind and
/// rain loops, reverb preset (events; 0: none) and its calls by band.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Region {
    pub index: u8,
    pub name: String,
    pub bed: u32,
    pub day_tracks: u32,
    pub night_tracks: u32,
    pub volume: f32,
    pub wind: u32,
    pub rain: u32,
    pub reverb: u32,
    pub calls: [Calls; 4],
}

/// The regions, and how much each band calls by hour (hour, the bands'
/// intensities).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ambiences {
    pub regions: Vec<Region>,
    pub times: Vec<(f32, [f32; 4])>,
}

fn event(o: &Object, name: &str) -> u32 {
    o.str(name).and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).filter(|e| *e != u32::MAX).unwrap_or(0)
}

fn band(o: &Object) -> Option<usize> {
    let name = o.str("disName")?;
    BANDS.iter().position(|b| *b == name)
}

/// The regions from `soundregions_default.obj` (see `rooms::REGIONS_PATH`).
pub fn parse(bytes: &[u8]) -> Result<Ambiences> {
    let root = crate::fcb::parse(bytes)?;
    let list = root.child("SoundRegions").context("no SoundRegions")?;
    let mut times: Vec<(f32, [f32; 4])> = root
        .child("TimeCurve")
        .map(|c| {
            c.children_named("TimeEntry")
                .filter_map(|t| {
                    let fx = t.child("RandomFx")?;
                    let mut i = [0.0; 4];
                    for (k, b) in fx.children.iter().take(4).enumerate() {
                        i[k] = b.f32("fIntensity").unwrap_or(0.0);
                    }
                    Some((t.f32("fTime")?, i))
                })
                .collect()
        })
        .unwrap_or_default();
    times.sort_by(|a, b| a.0.total_cmp(&b.0));
    let regions = list
        .children
        .iter()
        .filter_map(|r| {
            let index = r.field("disUniqueId").and_then(|b| b.first().copied())?;
            let mut region = Region {
                index,
                name: r.str("sName").unwrap_or_default(),
                day_tracks: r.u32("iDayTrackCount").unwrap_or(0),
                night_tracks: r.u32("iNightTrackCount").unwrap_or(0),
                ..Default::default()
            };
            let Some(amb) = r.child("Ambiance") else { return Some(region) };
            region.bed = event(amb, "sndMultitrackSound");
            region.wind = event(amb, "sndWindSound");
            region.rain = event(amb, "sndRainSound");
            if let Some(c) = amb.child("RandomFxContainer") {
                c.walk(&mut |o, _| {
                    if let (Some(b), Some(e)) = (band(o), Some(event(o, "sndSoundId")).filter(|e| *e != 0)) {
                        if !region.calls[b].events.contains(&e) {
                            region.calls[b].events.push(e);
                        }
                    }
                });
            }
            // (Its quietest level: the others are for when it's busier.)
            let level =
                amb.child("Levels").and_then(|l| l.children_named("Level").min_by_key(|l| l.field("iIntensity").and_then(|b| b.first().copied()).unwrap_or(0)));
            if let Some(level) = level {
                region.reverb = event(level, "sndReverb");
                region.volume = level.child("Volume").and_then(|v| v.f32("fVolume")).unwrap_or(0.0);
                if let Some(fx) = level.child("RandomFx") {
                    for o in &fx.children {
                        let Some(b) = band(o) else { continue };
                        let calls = &mut region.calls[b];
                        calls.per_minute = o.f32("fFrequency").unwrap_or(0.0);
                        calls.polyphony = o.u32("iPolyphony").unwrap_or(0);
                        calls.radius = (o.f32("fMinRadius").unwrap_or(0.0), o.f32("fMaxRadius").unwrap_or(0.0));
                    }
                }
            }
            Some(region)
        })
        .collect();
    Ok(Ambiences { regions, times })
}

impl Ambiences {
    pub fn region(&self, index: u8) -> Option<&Region> {
        self.regions.iter().find(|r| r.index == index)
    }

    /// How much each band calls at `hour` (between the curve's hours, and
    /// round from its last to its first).
    pub fn intensity(&self, hour: f32) -> [f32; 4] {
        let t = &self.times;
        let Some(&(first, a)) = t.first() else { return [1.0; 4] };
        let hour = hour.rem_euclid(24.0);
        let (from, to) = match t.iter().position(|e| e.0 > hour) {
            Some(0) | None => (*t.last().unwrap(), (first + 24.0, a)),
            Some(i) => (t[i - 1], t[i]),
        };
        let h = if hour < from.0 { hour + 24.0 } else { hour };
        let k = if to.0 > from.0 { ((h - from.0) / (to.0 - from.0)).clamp(0.0, 1.0) } else { 0.0 };
        std::array::from_fn(|b| from.1[b] + (to.1[b] - from.1[b]) * k)
    }

    /// `time <hour> <intensities>`, `region <index> <bed> <day tracks>
    /// <night tracks> <volume> <wind> <rain> <reverb> <name>` and `calls
    /// <index> <band> <per minute> <polyphony> <min> <max> <events>`.
    pub fn to_text(&self) -> String {
        let mut s = format!("{TABLE_MAGIC}\n");
        for (h, i) in &self.times {
            s += &format!("time {h} {} {} {} {}\n", i[0], i[1], i[2], i[3]);
        }
        for r in &self.regions {
            s += &format!(
                "region {} {:08x} {} {} {} {:08x} {:08x} {:08x} {}\n",
                r.index, r.bed, r.day_tracks, r.night_tracks, r.volume, r.wind, r.rain, r.reverb, r.name
            );
            for (b, c) in r.calls.iter().enumerate() {
                if c.events.is_empty() {
                    continue;
                }
                let events: Vec<String> = c.events.iter().map(|e| format!("{e:08x}")).collect();
                s += &format!("calls {} {b} {} {} {} {} {}\n", r.index, c.per_minute, c.polyphony, c.radius.0, c.radius.1, events.join(","));
            }
        }
        s
    }

    pub fn is_current(text: &str) -> bool {
        text.lines().next() == Some(TABLE_MAGIC)
    }

    pub fn from_text(text: &str) -> Result<Ambiences> {
        let mut lines = text.lines();
        if lines.next() != Some(TABLE_MAGIC) {
            bail!("not a sound region table");
        }
        let mut out = Ambiences::default();
        let hex = |s: &str| u32::from_str_radix(s, 16).context("event");
        for line in lines {
            let mut it = line.splitn(10, ' ');
            match it.next() {
                Some("time") => {
                    let v: Vec<f32> = it.map(|v| v.parse::<f32>()).collect::<Result<_, _>>()?;
                    if v.len() != 5 {
                        bail!("time line: {line}");
                    }
                    out.times.push((v[0], [v[1], v[2], v[3], v[4]]));
                }
                Some("region") => {
                    let mut f = || it.next().context("region line");
                    let index = f()?.parse()?;
                    let bed = hex(f()?)?;
                    let (day_tracks, night_tracks, volume) = (f()?.parse()?, f()?.parse()?, f()?.parse()?);
                    let (wind, rain, reverb) = (hex(f()?)?, hex(f()?)?, hex(f()?)?);
                    let name = it.next().unwrap_or_default().to_string();
                    out.regions.push(Region { index, name, bed, day_tracks, night_tracks, volume, wind, rain, reverb, calls: Default::default() });
                }
                Some("calls") => {
                    let mut f = || it.next().context("calls line");
                    let (index, band): (u8, usize) = (f()?.parse()?, f()?.parse()?);
                    let calls = Calls {
                        per_minute: f()?.parse()?,
                        polyphony: f()?.parse()?,
                        radius: (f()?.parse()?, f()?.parse()?),
                        events: f()?.split(',').map(hex).collect::<Result<_>>()?,
                    };
                    let region = out.regions.iter_mut().rev().find(|r| r.index == index).context("calls before their region")?;
                    *region.calls.get_mut(band).context("band")? = calls;
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sector_reads_its_cells_from_its_south_west_corner() {
        let mut cells = vec![7u8; CELLS * CELLS];
        // The cell 3 east, 5 north of the corner.
        cells[5 * CELLS + 3] = 9;
        let c = SectorCoord::new(80, 80);
        let uniform = SectorCoord::new(81, 80);
        let map = RegionMap::from_sectors(CELLS, [(c, cells), (uniform, vec![4u8; CELLS * CELLS])]);
        let map = RegionMap::from_bytes(&map.to_bytes()).unwrap();
        let [ox, oy] = c.origin();
        let cell = SECTOR_SIZE / CELLS as f32;
        assert_eq!(map.region_at(ox + 3.0 * cell + 1.0, oy + 5.0 * cell + 1.0), Some(9));
        assert_eq!(map.region_at(ox + 1.0, oy + 1.0), Some(7));
        assert_eq!(map.region_at(ox + SECTOR_SIZE + 10.0, oy + 30.0), Some(4));
        assert_eq!(map.region_at(ox - 10.0, oy), None);
        assert_eq!(map.counts(), (2, 1));
        // A zone map's 1 m cells, the unpainted ones none.
        let mut zones = vec![NONE; ZONE_CELLS * ZONE_CELLS];
        zones[2 * ZONE_CELLS + 63] = 20;
        let map = RegionMap::from_bytes(&RegionMap::from_sectors(ZONE_CELLS, [(c, zones)]).to_bytes()).unwrap();
        assert_eq!(map.region_at(ox + 63.5, oy + 2.5), Some(20));
        assert_eq!(map.region_at(ox + 62.5, oy + 2.5), None);
    }

    #[test]
    fn the_bands_follow_the_hours_round_midnight() {
        let a = Ambiences { regions: vec![], times: vec![(1.0, [0.0, 0.0, 1.0, 0.5]), (7.0, [1.0, 1.0, 0.0, 0.0]), (20.0, [0.0, 0.0, 1.0, 1.0])] };
        let near = |h: f32, want: [f32; 4]| {
            let got = a.intensity(h);
            assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-5), "{h}: {got:?}");
        };
        near(4.0, [0.5, 0.5, 0.5, 0.25]);
        near(7.0, [1.0, 1.0, 0.0, 0.0]);
        // 22:30, half way from 20:00 to 01:00.
        near(22.5, [0.0, 0.0, 1.0, 0.75]);
        near(0.0, [0.0, 0.0, 1.0, 0.6]);
    }

    #[test]
    fn the_table_reads_back() {
        let mut r = Region {
            index: 44,
            name: "A Region".into(),
            bed: 0x1133BD,
            day_tracks: 4,
            night_tracks: 2,
            volume: -6.0,
            wind: 0x599B,
            rain: 0x4B1969,
            reverb: 0x470ED,
            ..Default::default()
        };
        r.calls[3] = Calls { events: vec![0xA6BA5, 0x159609], per_minute: 1.5, polyphony: 2, radius: (30.0, 120.0) };
        let a = Ambiences { regions: vec![r], times: vec![(1.0, [0.0, 0.0, 1.0, 0.5])] };
        assert_eq!(Ambiences::from_text(&a.to_text()).unwrap(), a);
    }
}
