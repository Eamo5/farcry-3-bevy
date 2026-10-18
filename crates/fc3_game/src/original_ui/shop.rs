//! The shop page (`shop.feu`, quick launch 0), the original game's in place
//! of ours: at a shop (`GameState::Shop`) the pause menu opens on it. It
//! asks for one list at a time (`onRequestedMenu <id> <list>`), each
//! answered from the game's own shop tree (`fc3_formats::shop`) and our
//! stock (`progression`'s shop):
//!
//! * `DataCategory`: buy and sell (the page opens buy).
//! * `DataSub1Category <category>`: its subcategories, those with anything
//!   of ours in them, each with what it opens (`iTypeID`: guns, melee
//!   weapons, ammunition, other things).
//! * `DataWeapon <subcategory>`: its guns, with the shop's bars and stars.
//! * `DataItem <subcategory>`: its ammunition, supplies or maps, with how
//!   many are carried (`iCurrentCount`, of `iMaxCount`).
//! * `DataWidgetCustom <weapon>` the weapon's two customising lists, then
//!   `DataCustomization <list>` its attachments or its paints.
//! * `DataSell`: a tile for each thing in the rucksack, then one for each
//!   room left in it (`C_STATE_LOCK`).
//!
//! `OnBuy <type> <id>` buys (`progression::trade`), `EquipAttachment <id>`
//! puts a sight bought before back on or takes a paint off, `SellLoot <id>`
//! sells one of a thing and `OnSellAll` the valuables; each has the list
//! sent again, and the money (`SetMoney`). A tutorial's shop
//! (`ShopOverride`) opens on its weapon (`OverrideShopMenu`). The weapon's
//! 3D views are the engine's drawing; they're hidden.
//!
//! The engine answered a request at once, and the page counts on it: as it
//! hears of a weapon's customising lists it reads which weapon from where
//! it stood before the request (the breadcrumb, the stats). So the lists of
//! the weapons shown are ready before they're asked for
//! (`Host::prepare_data`), each weapon's two with ids of their own.

use std::collections::BTreeMap;

use bevy::prelude::*;
use fc3_formats::shop::{item_type, Category, SubCategory, Weapon};
use fc3_ui::ExternalValue as V;

use super::Interface;
use crate::progression::{ShopItem, Supply};
use crate::weapons::{Attachment, WeaponClass, PAINT_NAMES};

/// The page's calls this frame (`drive_pause` hands them on), the list it
/// shows (sent again after a trade), the weapon it's customising and the
/// game's shop tree.
#[derive(Resource, Default)]
pub(super) struct ShopPage {
    pub(super) calls: Vec<fc3_ui::EngineCall>,
    shown: Option<(String, i64)>,
    /// The subcategory whose guns are shown, and the weapon customised.
    sub: Option<i64>,
    weapon: Option<usize>,
    tree: Option<(Vec<Category>, Vec<SubCategory>)>,
    tried: bool,
    /// A tutorial's weapon to open on, once the subcategories are in.
    override_pending: bool,
    /// The gun the equip wheel is putting in a holster.
    equipping: Option<usize>,
    equip_tested: bool,
}

/// A button's kind (`C_ButtonShop.C_TYPE_*`).
const WEAPON: f64 = 0.0;
const ATTACHMENT: f64 = 1.0;
const ITEM: f64 = 2.0;
const AMMO: f64 = 4.0;
const MELEE: f64 = 5.0;
const PAINT: f64 = 6.0;
/// A button's state (`C_Button.C_STATE_*`).
const LOCK: f64 = 0.0;
const AVAILABLE: f64 = 1.0;
const BUY: f64 = 2.0;
const BOUGHT: f64 = 3.0;

/// The customising lists' ids (the widget's buttons): past the shop's
/// own, a weapon's attachments and its paints.
const CUSTOM: i64 = 1_000_000;
/// A paint's id (`iNameID`), past the attachments' bits.
const PAINT_ID: i64 = 1 << 20;

fn custom_id(weapon: u32, paints: bool) -> i64 {
    CUSTOM + i64::from(weapon) * 2 + i64::from(paints)
}

/// A customising list's weapon (its shop id) and whether it's the paints.
fn custom_of(id: i64) -> Option<(i64, bool)> {
    (id >= CUSTOM).then(|| ((id - CUSTOM) / 2, (id - CUSTOM) % 2 == 1))
}
/// The lists' names (string ids) and icons.
const ATTACHMENTS_NAME: u32 = 155655;
const PAINTS_NAME: u32 = 155656;

/// The 3D views of the weapon and item (DPS ids).
const VIEWS: [&str; 11] = [
    "3D_MC",
    "Loading_MC",
    "3D_Weapon_MC",
    "3D_Weapon_MC_B",
    "3D_Weapon_MC_C",
    "3D_Weapon_MC_D",
    "Secondary3D",
    "Secondary3D_B",
    "Secondary3D_C",
    "Secondary3D_D",
    "3D_Item_MC",
];

type Row = BTreeMap<String, V>;

fn row(fields: Vec<(&str, V)>) -> Row {
    fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn n(v: impl Into<f64>) -> V {
    V::Number(v.into())
}

/// A statistic (out of 5) as the page's bars take it (0 to 1).
fn bar(v: f32) -> f32 {
    (v / 5.0).clamp(0.0, 1.0)
}

/// What the lists are made from.
struct Stock<'a> {
    host: &'a fc3_ui::Host,
    tree: &'a (Vec<Category>, Vec<SubCategory>),
    inv: &'a crate::weapons::Inventory,
    stats: &'a crate::combat::PlayerStats,
    progress: &'a crate::progression::Progress,
    perks: &'a crate::progression::Perks,
    catalog: &'a crate::progression::ShopCatalog,
    crafting: &'a crate::crafting::CraftCatalog,
    loot: Option<&'a crate::progression::LootCatalog>,
    maps: &'a crate::treasure_maps::TreasureMaps,
}

impl Stock<'_> {
    /// A string, or ours where the game's is blank.
    fn text(&self, id: u32, or: &str) -> String {
        Some(self.host.text(id)).filter(|t| !t.is_empty()).unwrap_or_else(|| or.to_string())
    }

    /// A subcategory's guns: the shop's that are ours (by model), once each.
    fn weapons_in(&self, sub: u32) -> Vec<(&Weapon, usize)> {
        let mut out: Vec<(&Weapon, usize)> = Vec::new();
        for w in self.catalog.0.iter().filter(|w| w.category == sub && w.is_base()) {
            let Some(def) = self.inv.defs.iter().position(|d| d.model_path().eq_ignore_ascii_case(&w.model)) else { continue };
            if !out.iter().any(|(_, d)| *d == def) {
                out.push((w, def));
            }
        }
        out
    }

    fn weapon_def(&self, id: i64) -> Option<usize> {
        let w = self.catalog.0.iter().find(|w| i64::from(w.id) == id && w.is_base())?;
        self.inv.defs.iter().position(|d| d.model_path().eq_ignore_ascii_case(&w.model))
    }

    /// What a subcategory's button opens: guns, melee weapons, ammunition or
    /// other things.
    fn kind(&self, sub: &SubCategory) -> f64 {
        if sub.item_types.contains(&item_type::AMMO) {
            return AMMO;
        }
        let guns = self.weapons_in(sub.id);
        if guns.is_empty() {
            ITEM
        } else if guns.iter().all(|(_, d)| self.inv.defs[*d].class == WeaponClass::Melee) {
            MELEE
        } else {
            WEAPON
        }
    }

    fn subcategories(&self, category: i64) -> Vec<Row> {
        let Some(c) = self.tree.0.iter().find(|c| i64::from(c.id) == category) else { return Vec::new() };
        let mut subs: Vec<&SubCategory> = self.tree.1.iter().filter(|s| c.subcategories.contains(&s.id)).collect();
        subs.sort_by_key(|s| (s.sort, s.id));
        subs.into_iter()
            .filter_map(|s| {
                let kind = self.kind(s);
                let empty = match kind {
                    WEAPON => self.weapons_in(s.id).is_empty(),
                    _ => self.items(s.id).is_empty(),
                };
                (!empty).then(|| {
                    row(vec![("sName", V::String(self.host.text(s.name_id))), ("iObjectID", n(s.id)), ("iTypeID", n(kind)), ("iStateID", n(AVAILABLE))])
                })
            })
            .collect()
    }

    fn in_holster(&self, def: usize) -> bool {
        self.inv.slots.iter().any(|s| s.def == def)
    }

    /// Carried, or bought and put by.
    fn owned(&self, def: usize) -> bool {
        self.in_holster(def) || self.progress.owned.contains(self.inv.defs[def].name)
    }

    /// The equip wheel (`DataEquip`): the four holsters (the guns carried,
    /// in order; a place without one open while the holsters have room,
    /// locked beyond), and the gun to put in one.
    fn equip(&self, def: usize) -> Vec<Row> {
        let shown = |d: usize| match self.catalog.weapon(self.inv, d) {
            Some(w) => (self.text(w.name_id, self.inv.defs[d].name), w.icon.clone()),
            None => (self.inv.defs[d].name.to_string(), String::new()),
        };
        let guns: Vec<usize> = self.inv.slots.iter().map(|s| s.def).filter(|&d| self.inv.defs[d].class != crate::weapons::WeaponClass::Melee).collect();
        let mut rows: Vec<Row> = (0..crate::weapon_wheel::PLACES)
            .map(|p| {
                let gun = guns.get(p).copied();
                let (name, image) = gun.map(shown).unwrap_or_default();
                let state = if p >= self.perks.weapon_slots.max(1) {
                    LOCK
                } else if gun.is_some() {
                    BOUGHT
                } else {
                    AVAILABLE
                };
                row(vec![("sName", V::String(name)), ("sImage", V::String(image)), ("iStateID", n(state)), ("iObjectID", n(p as f64))])
            })
            .collect();
        let (name, image) = shown(def);
        rows.push(row(vec![("sName", V::String(name)), ("sImage", V::String(image)), ("iStateID", n(BOUGHT))]));
        rows
    }

    /// A gun owned that takes an attachment or a paint.
    fn customisable(&self, def: usize) -> bool {
        let paints = self.inv.paint_options.get(def).is_some_and(|b| *b != 0);
        self.owned(def) && (Attachment::ALL.iter().any(|a| a.fits(&self.inv.defs[def])) || paints)
    }

    /// A gun (or melee weapon) as the lists show it.
    fn weapon(&self, w: &Weapon, def: usize, kind: f64) -> Row {
        let d = &self.inv.defs[def];
        let owned = self.owned(def);
        let price = ShopItem::Weapon(def).price(self.inv, self.progress.towers.len(), self.catalog);
        let custom = kind == WEAPON && self.customisable(def);
        row(vec![
            ("sName", V::String(self.text(w.name_id, d.name))),
            ("sInfo", V::String(self.host.text(w.description_id))),
            ("sImage", V::String(w.icon.clone())),
            ("iObjectID", n(w.id)),
            ("iTypeID", n(kind)),
            ("iStateID", n(if owned { BOUGHT } else { BUY })),
            ("iPrice", n(price)),
            ("iAccuracy", n(bar(w.stats[0]))),
            ("iDamage", n(bar(w.stats[1]))),
            ("iRange", n(bar(w.stats[2]))),
            ("iFireRate", n(bar(w.stats[3]))),
            ("iHandling", n(bar(w.stats[4]))),
            ("fRating", n(w.rating)),
            ("bCustomizable", V::Bool(custom)),
            // (In a holster; a melee weapon owned is the one in hand.)
            ("bEquip", V::Bool(self.in_holster(def))),
            ("bNew", V::Bool(false)),
            ("bChallenge", V::Bool(false)),
        ])
    }

    fn weapons(&self, sub: i64) -> Vec<Row> {
        let Ok(sub) = u32::try_from(sub) else { return Vec::new() };
        self.weapons_in(sub).into_iter().map(|(w, def)| self.weapon(w, def, WEAPON)).collect()
    }

    /// How many of a supply are carried, of how many, and whether that's a
    /// percentage (body armour's).
    fn carried(&self, item: &fc3_formats::progression::Item) -> Option<(u32, u32, bool)> {
        let (inv, perks) = (self.inv, self.perks);
        Some(match crate::progression::supply(item)? {
            Supply::Ammo(class) => inv
                .slots
                .iter()
                .filter(|s| inv.defs[s.def].class == class)
                .fold((0, 0, false), |(have, cap, p), s| (have + s.reserve, cap + (inv.defs[s.def].max_reserve as f32 * inv.reserve_scale) as u32, p)),
            Supply::Grenade => (inv.grenades, perks.grenade_cap, false),
            Supply::Molotov => (inv.molotovs, crate::weapons::MOLOTOV_CAP, false),
            Supply::C4 => (inv.c4, perks.explosive_cap, false),
            Supply::Mine => (inv.mines, perks.explosive_cap, false),
            Supply::Medkit => (self.stats.medkits, perks.medkit_cap, false),
            Supply::Armour(value) if value > 0.0 => (((self.progress.armour / value).min(1.0) * 100.0).round() as u32, 100, true),
            Supply::Armour(_) | Supply::RefillAll => return None,
        })
    }

    /// A subcategory's things: its melee weapons, its maps, or its
    /// supplies.
    fn items(&self, sub: u32) -> Vec<Row> {
        let Some(s) = self.tree.1.iter().find(|s| s.id == sub) else { return Vec::new() };
        if s.item_types.contains(&item_type::MAP) {
            return self
                .maps
                .for_sale
                .iter()
                .filter_map(|&(id, cost)| {
                    let m = self.maps.maps.iter().find(|m| m.id == id)?;
                    Some(row(vec![
                        ("sName", V::String(self.text(m.name_id, &self.maps.label(id)))),
                        ("sInfo", V::String(self.host.text(m.description_id))),
                        ("iObjectID", n(id)),
                        ("iTypeID", n(ITEM)),
                        ("iStateID", n(if self.progress.maps.contains(&id) { BOUGHT } else { BUY })),
                        ("iPrice", n(cost)),
                    ]))
                })
                .collect();
        }
        let melee: Vec<Row> = self
            .weapons_in(sub)
            .into_iter()
            .filter(|(_, d)| self.inv.defs[*d].class == WeaponClass::Melee)
            .map(|(w, def)| self.weapon(w, def, MELEE))
            .collect();
        if !melee.is_empty() {
            return melee;
        }
        let kind = if s.item_types.contains(&item_type::AMMO) { AMMO } else { ITEM };
        crate::progression::supply_items(self.inv, &self.crafting.items, self.progress)
            .into_iter()
            .filter_map(|line| {
                let ShopItem::Supply(id, price) = line else { return None };
                let item = self.crafting.items.iter().find(|i| i.id == id && i.shop == sub)?;
                let name = if item.name.is_empty() { item.key.rsplit('/').next().unwrap_or_default().to_string() } else { item.name.clone() };
                let mut r = row(vec![
                    ("sName", V::String(name)),
                    ("sInfo", V::String(item.description.clone())),
                    ("sImage", V::String(item.icon.clone())),
                    ("iObjectID", n(id)),
                    ("iTypeID", n(kind)),
                    ("iStateID", n(BUY)),
                    ("iPrice", n(price)),
                ]);
                if let Some((have, cap, percent)) = self.carried(item) {
                    r.insert("iCurrentCount".into(), n(have));
                    r.insert("iMaxCount".into(), n(cap));
                    r.insert("bShowItemCountAsPercent".into(), V::Bool(percent));
                }
                Some(r)
            })
            .collect()
    }

    /// A weapon's two customising lists.
    fn widget(&self, weapon: u32) -> Vec<Row> {
        [(false, ATTACHMENTS_NAME, "TX_Attachment"), (true, PAINTS_NAME, "TX_Skin")]
            .into_iter()
            .map(|(paints, name, icon)| {
                row(vec![("sName", V::String(self.host.text(name))), ("iObjectID", n(custom_id(weapon, paints) as f64)), ("sImage", V::String(icon.into()))])
            })
            .collect()
    }

    /// A weapon's attachments, or its paints.
    fn customisation(&self, def: usize, paints: bool) -> Vec<Row> {
        let Some(d) = self.inv.defs.get(def) else { return Vec::new() };
        let towers = self.progress.towers.len();
        if paints {
            let bits = self.inv.paint_options.get(def).copied().unwrap_or(0);
            let worn = self.inv.paints.get(def).copied().flatten();
            return (0..PAINT_NAMES.len() as u8)
                .filter(|p| bits & (1 << p) != 0)
                .map(|p| {
                    row(vec![
                        ("sName", V::String(PAINT_NAMES[p as usize].to_string())),
                        ("iObjectID", n(p)),
                        ("iNameID", n((PAINT_ID + i64::from(p)) as f64)),
                        ("iTypeID", n(PAINT)),
                        ("iStateID", n(if worn == Some(p) { BOUGHT } else { BUY })),
                        ("iPrice", n(ShopItem::Paint(def, p).price(self.inv, towers, self.catalog))),
                        ("bEquip", V::Bool(worn == Some(p))),
                    ])
                })
                .collect();
        }
        Attachment::ALL
            .into_iter()
            .filter(|a| a.fits(d))
            .map(|a| {
                let offer = a.offer(d);
                let name = offer.map_or(String::new(), |o| self.host.text(o.name_id));
                let name = if name.is_empty() { a.name(d).to_string() } else { name };
                let info = offer.map_or(String::new(), |o| self.host.text(o.description_id));
                // (Sights go in one place: the page compares them with each other.)
                let part = if a.is_sight() { 0 } else { a.bit() };
                row(vec![
                    ("sName", V::String(name)),
                    ("sInfo", V::String(info)),
                    ("sImage", V::String(offer.map_or(String::new(), |o| o.icon.clone()))),
                    ("iObjectID", n(a.bit())),
                    ("iNameID", n(a.bit())),
                    ("iTypeID", n(ATTACHMENT)),
                    ("iStateID", n(if self.inv.owns(def, a) { BOUGHT } else { BUY })),
                    ("iPrice", n(ShopItem::Attachment(def, a).price(self.inv, towers, self.catalog))),
                    ("bEquip", V::Bool(self.inv.has_attachment(def, a))),
                    ("iPart", n(part)),
                    ("iAccuracy", n(0)),
                    ("iDamage", n(0)),
                    ("iRange", n(0)),
                    ("iFireRate", n(0)),
                    ("iHandling", n(0)),
                    ("fRating", n(0)),
                ])
            })
            .collect()
    }

    /// The customising lists of the guns of a subcategory, ready for the
    /// page (and no others).
    fn prepare(&self, sub: Option<i64>) {
        self.host.prepare_data("", Vec::new());
        let Some(sub) = sub.and_then(|s| u32::try_from(s).ok()) else { return };
        for (w, def) in self.weapons_in(sub) {
            if !self.customisable(def) {
                continue;
            }
            self.host.prepare_data(&format!("DataWidgetCustom:{}", w.id), self.widget(w.id));
            for paints in [false, true] {
                self.host.prepare_data(&format!("DataCustomization:{}", custom_id(w.id, paints)), self.customisation(def, paints));
            }
        }
    }

    /// The rucksack's things, then its room left.
    fn sell(&self) -> Vec<Row> {
        let mut tiles = super::rucksack_tiles(self.progress, Some(self.crafting), self.loot, self.perks);
        let room = self.crafting.loot_capacity(self.progress) as usize;
        while tiles.len() < room {
            tiles.push(row(vec![("sName", V::String(String::new())), ("iObjectID", n(-1)), ("iStateID", n(LOCK))]));
        }
        tiles
    }
}

/// Answers the shop page's calls.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn drive_shop_page(
    mut state: NonSendMut<Interface>,
    mut page: ResMut<ShopPage>,
    info: Res<crate::data::CacheInfo>,
    mut players: Query<(&mut crate::weapons::Inventory, &mut crate::combat::PlayerStats), With<crate::player::Player>>,
    (mut progress, perks, catalog, restriction): (
        ResMut<crate::progression::Progress>,
        Res<crate::progression::Perks>,
        Res<crate::progression::ShopCatalog>,
        Res<crate::progression::ShopOverride>,
    ),
    (crafting, loot, maps): (Option<Res<crate::crafting::CraftCatalog>>, Option<Res<crate::progression::LootCatalog>>, Res<crate::treasure_maps::TreasureMaps>),
    mut hud: ResMut<crate::hud::HudState>,
    mut bought: MessageWriter<crate::progression::ItemBought>,
) {
    let Interface::Running(host) = &mut *state else { return };
    if page.calls.is_empty() {
        return;
    }
    if page.tree.is_none() && !page.tried {
        page.tried = true;
        let read = |rel: &str| std::fs::read(info.root.join(rel));
        match read(fc3_formats::shop::CATEGORIES_REL)
            .and_then(|c| Ok((c, read(fc3_formats::shop::SUBCATEGORIES_REL)?)))
            .map_err(anyhow::Error::from)
            .and_then(|(c, s)| fc3_formats::shop::tree_from_libs(&c, &s))
        {
            Ok(tree) => page.tree = Some(tree),
            Err(e) => warn!("original UI: the shop's tree: {e:#}"),
        }
    }
    let calls = std::mem::take(&mut page.calls);
    let (Ok((mut inv, mut stats)), Some(crafting)) = (players.single_mut(), crafting.as_deref()) else { return };
    let mut tree = page.tree.take();
    let empty = (Vec::new(), Vec::new());
    macro_rules! stock {
        () => {
            Stock {
                host,
                tree: tree.as_ref().unwrap_or(&empty),
                inv: &inv,
                stats: &stats,
                progress: &progress,
                perks: &perks,
                catalog: &catalog,
                crafting,
                loot: loot.as_deref(),
                maps: &maps,
            }
        };
    }
    let mut equip_test = false;
    for call in calls {
        let arg = |i: usize| call.args.get(i).map(String::as_str).unwrap_or("");
        let number = |i: usize| arg(i).parse::<f64>().map_or(-1, |v| v as i64);
        // The list to send again, and whether the money and the lists made
        // ready may have changed.
        let mut resend = false;
        let mut changed = false;
        match arg(0) {
            "ShopManagerLoaded" => {
                for view in VIEWS {
                    host.dps_visible(view, false);
                }
                page.override_pending = restriction.0.is_some();
                page.weapon = None;
                page.shown = None;
                page.sub = None;
                changed = true;
            }
            "onRequestedMenu" => {
                let list = arg(2).to_string();
                let mut id = number(1);
                let s = stock!();
                match list.as_str() {
                    "DataWeapon" => page.sub = Some(id),
                    "DataWidgetCustom" => page.weapon = s.weapon_def(id),
                    // (A weapon's own id asks for its attachments.)
                    "DataCustomization" if id >= CUSTOM => page.weapon = custom_of(id).and_then(|(w, _)| s.weapon_def(w)),
                    "DataCustomization" => {
                        page.weapon = s.weapon_def(id);
                        id = custom_id(u32::try_from(id).unwrap_or(0), false);
                    }
                    _ => {}
                }
                page.shown = Some((list, id));
                // (One made ready went to the page as it asked.)
                resend = arg(3) != "prepared";
                changed = resend;
            }
            "OnBuy" => {
                let kind = arg(1).parse::<f64>().unwrap_or(-1.0);
                let id = number(2);
                let s = stock!();
                let item = if kind == WEAPON || kind == MELEE {
                    s.weapon_def(id).map(ShopItem::Weapon)
                } else if kind == ATTACHMENT {
                    let a = Attachment::ALL.into_iter().find(|a| i64::from(a.bit()) == id);
                    page.weapon.zip(a).map(|(d, a)| ShopItem::Attachment(d, a))
                } else if kind == PAINT {
                    page.weapon.zip(u8::try_from(id).ok()).map(|(d, p)| ShopItem::Paint(d, p))
                } else if let Some(&(map, cost)) = maps.for_sale.iter().find(|(m, _)| i64::from(*m) == id) {
                    Some(ShopItem::Map(map, cost))
                } else {
                    crate::progression::supply_items(&inv, &crafting.items, &progress)
                        .into_iter()
                        .find(|l| matches!(l, ShopItem::Supply(s, _) if i64::from(*s) == id))
                };
                // (A gun with every holster full is bought to own, and the
                // equip wheel opens for it.)
                if let Some(ShopItem::Weapon(def)) = item.filter(|_| kind == WEAPON && crate::progression::holsters_full(&inv, &perks)) {
                    if let Some(b) = crate::progression::buy_to_own(&inv, &mut progress, &catalog, def) {
                        bought.write(b);
                        page.equipping = Some(def);
                        let s = stock!();
                        host.set_data("DataEquip", s.equip(def));
                    }
                    (resend, changed) = (true, true);
                } else if let Some(item) = item {
                    if let Some(b) = crate::progression::trade(item, &mut inv, &mut stats, &mut progress, &perks, &catalog, crafting, &mut hud) {
                        bought.write(b);
                    }
                    (resend, changed) = (true, true);
                }
            }
            // A gun owned to put in a holster: the equip wheel.
            "OnEquipRequest" => {
                let s = stock!();
                if let Some(def) = s.weapon_def(number(1)).filter(|&d| s.owned(d) && !s.in_holster(d)) {
                    host.set_data("DataEquip", s.equip(def));
                    page.equipping = Some(def);
                }
            }
            // The holster chosen: the gun goes in it (the one there put by).
            "OnWeaponEquip" => {
                let Some(def) = page.equipping.take() else { continue };
                let place = usize::try_from(number(1)).unwrap_or(usize::MAX);
                if crate::weapons::equip_gun(&mut inv, def, place, perks.weapon_slots) {
                    info!("shop: equipped {} in holster {place}", inv.defs[def].name);
                }
                host.event("HideEquip", &[]);
                (resend, changed) = (true, true);
            }
            // A sight bought before back on, or a paint taken off (or put on).
            "EquipAttachment" => {
                let id = number(1);
                let Some(def) = page.weapon else { continue };
                if id >= PAINT_ID {
                    let p = (id - PAINT_ID) as u8;
                    if inv.paints.get(def).copied().flatten() == Some(p) {
                        inv.paints[def] = None;
                    } else if let Some(b) =
                        crate::progression::trade(ShopItem::Paint(def, p), &mut inv, &mut stats, &mut progress, &perks, &catalog, crafting, &mut hud)
                    {
                        bought.write(b);
                    }
                } else if let Some(a) = Attachment::ALL.into_iter().find(|a| i64::from(a.bit()) == id) {
                    if inv.owns(def, a) && !inv.has_attachment(def, a) {
                        crate::progression::trade(ShopItem::Attachment(def, a), &mut inv, &mut stats, &mut progress, &perks, &catalog, crafting, &mut hud);
                    }
                }
                (resend, changed) = (true, true);
            }
            // One of a thing sold.
            "SellLoot" => {
                let Some(loot) = loot.as_deref() else { continue };
                let Ok(item) = u32::try_from(number(1)) else { continue };
                let before = crate::crafting::loot_count(&progress);
                super::drop_loot(&mut progress, loot, item);
                if crate::crafting::loot_count(&progress) < before {
                    progress.add_money(crafting.sale_price(item, &perks));
                }
                (resend, changed) = (true, true);
            }
            "OnSellAll" => {
                crate::progression::trade(ShopItem::SellAll, &mut inv, &mut stats, &mut progress, &perks, &catalog, crafting, &mut hud);
                (resend, changed) = (true, true);
            }
            "SortSell" | "OnMeleeEquip" | "onSeen" | "onRequestWeapon3D" | "onRequestItem3D" | "onRequestAttachment3D" | "onRequestCategory3D" => {}
            other => debug!("original UI: shop page {other} {:?} not handled yet", call.args.get(1..)),
        }
        let s = stock!();
        if let Some((list, id)) = page.shown.clone().filter(|_| resend) {
            let rows = match list.as_str() {
                "DataCategory" => s
                    .tree
                    .0
                    .iter()
                    .map(|c| {
                        row(vec![
                            ("sName", V::String(host.text(c.name_id))),
                            ("iObjectID", n(c.id)),
                            ("sImage", V::String(c.icon.clone())),
                            ("iStateID", n(AVAILABLE)),
                        ])
                    })
                    .collect(),
                "DataSub1Category" => s.subcategories(id),
                "DataWeapon" => s.weapons(id),
                "DataItem" => u32::try_from(id).map_or(Vec::new(), |sub| s.items(sub)),
                "DataWidgetCustom" => u32::try_from(id).map_or(Vec::new(), |w| s.widget(w)),
                "DataCustomization" => page.weapon.map_or(Vec::new(), |d| s.customisation(d, custom_of(id).is_some_and(|(_, paints)| paints))),
                "DataSell" => s.sell(),
                _ => Vec::new(),
            };
            host.set_data(&list, rows);
            equip_test |= list == "DataSub1Category";
            // A tutorial's weapon, once the page has its subcategories.
            if list == "DataSub1Category" && page.override_pending {
                page.override_pending = false;
                if let Some((sub, weapon)) = restriction.0 {
                    host.event("OverrideShopMenu", &[&sub.to_string(), &weapon.to_string()]);
                }
            }
        }
        if changed {
            s.prepare(page.sub);
            host.event("SetMoney", &[&progress.money.to_string(), &progress.money_cap().to_string()]);
        }
    }
    // FC3_SHOP_EQUIP=<gun> (tests): that gun owned and the equip wheel open
    // for it, once the page has its subcategories.
    if let Some(name) = std::env::var("FC3_SHOP_EQUIP").ok().filter(|_| equip_test && !page.equip_tested) {
        page.equip_tested = true;
        if let Some(def) = inv.defs.iter().position(|d| d.name.eq_ignore_ascii_case(&name)) {
            progress.owned.insert(inv.defs[def].name.to_string());
            let s = stock!();
            host.set_data("DataEquip", s.equip(def));
            page.equipping = Some(def);
        }
    }
    page.tree = tree.take();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each weapon's two lists have ids of their own, past the shop's, and
    /// give the weapon back.
    #[test]
    fn customising_lists_are_known_by_their_weapon() {
        assert_eq!(custom_of(custom_id(83, false)), Some((83, false)));
        assert_eq!(custom_of(custom_id(83, true)), Some((83, true)));
        assert_ne!(custom_id(83, true), custom_id(84, false));
        assert_eq!(custom_of(1389), None);
    }

    /// The shop's statistics (out of 5) fill the page's bars (0 to 1).
    #[test]
    fn statistics_fill_the_bars() {
        assert_eq!((bar(4.0), bar(5.0), bar(0.0), bar(7.0)), (0.8, 1.0, 0.0, 1.0));
    }
}
