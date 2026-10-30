//! OFFLINE ITEM SHOP (substitute): the storefront the 1.8 client got from Epic's servers, rebuilt locally. The WIDGETS are the real cooked ones
//! (`fn-ui/src/screens/athena_shop`, ripped from `AthenaDirectAcquisitionScreen` and its offer / details / purchased widgets); only the DATA behind them is
//! substituted here, and every substitute is labelled:
//!
//! * CATALOG (substitute): the storefront's offers (`FortMcpContext` / `GetOfferInfo`) are server data and are NOT in the paks. This catalog is built only from
//!   cosmetic item definitions that exist in the 1.8 paks (`/Game/Athena/Items/Cosmetics/**`, read by `fn_pose::cosmetics`; outfits, harvesting tools, gliders;
//!   Battle Royale only, no Save the World item). One offer per item: offer id `offer:<item id>`, grants that item.
//!   Not sellable (stay obtainable through the console): development assets, the default items, founder / umbrella / pre-season rewards (not shop goods).
//! * DISPLAY ASSETS: `/Game/Catalog/DisplayAssets/DA_*` (`FortMtxOfferData`) ship in the paks (`TileImage`, `DetailsImage`). Which offer used which one was server
//!   data; [`DISPLAY_ASSET_FOR_ITEM`] maps the Battle Royale ones by name (each target is checked by a test). Items with a `DA_Featured_*` asset form the
//!   FEATURED pool of the weekly storefront (their art is the large featured tile art); every other item uses its own large preview image.
//! * PRICES (substitute): no price data exists in the paks (searched `/Game/Catalog`, `/Game/Items/Currency`, the item definitions and `FortMtxOfferData`:
//!   the display assets carry only images / colours). One documented table of the item's RARITY TIER, [`RARITY_PRICES`], applies to every item.
//! * ROTATION (substitute): deterministic and seeded by the UTC date. The DAILY storefront (6 small tiles, `SmallTileGP`: 2 rows, filled column-major by
//!   `GenerateOfferWidget`) rotates at 00:00 UTC; the WEEKLY storefront (2 large tiles, `LargeTileHB`) rotates every 7 days at 00:00 UTC on the Thursday
//!   boundary (Unix day 0 was a Thursday). Section sizes come from the widget layout: at 1920x1080 two 384-wide large tiles + three 256-wide small columns
//!   are what fits the 1920-unit `AthenaDirectAcquisitionScreen` (a third large tile would not).
//! * `Profile::shop_day_offset` shifts the date that seeds the rotation (console `rotateshop [days]`).
//!
//! No network, no account, no real commerce: a purchase only moves V-Bucks inside [`crate::lobby_profile::Profile`].
use crate::lobby_profile::{Profile, PurchaseError, PurchaseRequest, Receipt, Slot};
use fn_pose::cosmetics::{Catalog, Category};
use fn_ui::screens::athena_shop::view::{OfferView, ShopView};
use std::collections::HashMap;

/// Items per storefront (see the module docs: what the 1.8 widgets lay out).
pub const WEEKLY_SLOTS: usize = 2;
pub const DAILY_SLOTS: usize = 4;

/// OFFLINE PRICE TABLE, REFERENCE-DERIVED (no price exists in the 1.8 paks: searched `/Game/Catalog`, the item definitions and `FortMtxOfferData`). The item
/// definitions serialize the rarity as `EFortRarity::<internal name>` (Handmade..Masterwork = Common..Transcendent). Values are V-Bucks per (rarity, item type),
/// taken from a real 1.8 Halloween storefront screenshot: Reaper (rare pickaxe) 800, Lucky (uncommon pickaxe) 500, Petunia (uncommon glider) 500, Renegade
/// (uncommon outfit) 800, Ghoul Trooper (epic outfit) 1,500. Tiers the reference does not show (common, rare outfit / glider, epic pickaxe / glider and above) are
/// extrapolated along the same ladder and are NOT confirmed data. Columns: (internal name, rarity index, outfit, harvesting tool, glider).
pub const RARITY_PRICES: [(&str, usize, i64, i64, i64); 7] = [
    ("Handmade", 0, 500, 200, 200),     // Common
    ("Ordinary", 1, 800, 500, 500),     // Uncommon (reference: Renegade 800, Lucky 500, Petunia 500)
    ("Sturdy", 2, 1200, 800, 800),      // Rare (reference: Reaper 800)
    ("Quality", 3, 1500, 1200, 1200),   // Epic (reference: Ghoul Trooper 1500)
    ("Fine", 4, 2000, 1500, 1500),      // Legendary
    ("Elegant", 5, 2500, 2000, 2000),   // Mythic
    ("Masterwork", 6, 3000, 2500, 2500),// Transcendent
];
/// Single-item exceptions of the reference (same rarity as another item of the table but a different price).
pub const PRICE_OVERRIDES: [(&str, i64); 1] = [("CID_030_Athena_Commando_M_Halloween", 1200)]; // Skull Trooper: epic, 1,200 in the reference
pub fn rarity_index(internal_name: &str) -> usize {
    RARITY_PRICES.iter().find(|(n, ..)| n.eq_ignore_ascii_case(internal_name)).map(|(_, i, ..)| *i).unwrap_or(0)
}
pub fn item_price(id: &str, category: Category, rarity: usize) -> i64 {
    if let Some((_, p)) = PRICE_OVERRIDES.iter().find(|(i, _)| *i == id) { return *p; }
    let row = RARITY_PRICES.iter().find(|r| r.1 == rarity).unwrap_or(&RARITY_PRICES[0]);
    match category { Category::Pickaxe => row.3, Category::Glider => row.4, _ => row.2 }
}
pub fn rarity_title(rarity: usize) -> &'static str { ["Common", "Uncommon", "Rare", "Epic", "Legendary", "Mythic", "Transcendent"].get(rarity).copied().unwrap_or("Common") }

/// OFFLINE MAPPING (substitute): item id -> `/Game/Catalog/DisplayAssets/<name>` of the paks. Battle Royale items only (`DA_FoundersPack_*`, `DA_MtxPack*`,
/// `DA_AutumnBattleBook`, `DA_CID_Headhunter_01` belong to Save the World / V-Bucks packs and are not used).
pub const DISPLAY_ASSET_FOR_ITEM: [(&str, &str, bool); 9] = [
    // (item id, display asset, featured pool)
    ("Glider_Prismatic", "DA_Featured_GliderPrismatic", true),
    ("Glider_Voyager", "DA_Featured_GliderVoyager", true),
    ("Pickaxe_Flamingo", "DA_Featured_PickaxeFlamingo", true),
    ("HalloweenScythe", "DA_Featured_PickaxeHalloween", true),
    ("CID_029_Athena_Commando_F_Halloween", "DA_Featured_SFemaleHalloween", true),
    ("CID_030_Athena_Commando_M_Halloween", "DA_Featured_SMaleHalloween", true),
    ("CID_027_Athena_Commando_F", "DA_Featured_SFemaleHID027", true),
    ("BoltonPickaxe", "DA_BoltOnPickaxe", false),
    ("HappyPickaxe", "DA_HappyPickAxe", false),
];

#[derive(Clone, Debug, PartialEq)]
pub struct ShopItem {
    /// asset name of the item definition (`HalloweenScythe`)
    pub id: String,
    /// package of the item definition (always under `/Game/Athena/Items/Cosmetics`)
    pub package: String,
    pub name: String,
    pub short_description: String,
    pub description: String,
    pub category: Category,
    /// `RarityData` index
    pub rarity: usize,
    pub rarity_name: String,
    pub price: i64,
    /// texture object path of the `FortMtxOfferData.TileImage` when the item has a display asset, else None
    pub display_tile: Option<String>,
    /// texture object path of the item definition's large preview image (hero preview for outfits)
    pub large_icon: Option<String>,
    pub small_icon: Option<String>,
    /// belongs to the FEATURED (weekly) pool
    pub featured: bool,
    /// may appear in the shop rotation
    pub sellable: bool,
}

impl ShopItem {
    pub fn offer_id(&self) -> String { format!("offer:{}", self.id) }
    pub fn slot(&self) -> Option<Slot> { slot_of(self.category) }
}

pub fn slot_of(c: Category) -> Option<Slot> {
    match c { Category::Outfit => Some(Slot::Outfit), Category::Pickaxe => Some(Slot::Pickaxe), Category::Glider => Some(Slot::Glider), _ => None }
}

/// Not shop goods: defaults, founder / umbrella / pre-season rewards (obtainable through the console).
pub fn is_shop_good(id: &str) -> bool {
    let l = id.to_ascii_lowercase();
    !(l.contains("default") || l.contains("founder") || l.contains("umbrella") || l.contains("preseason"))
}

#[derive(Clone, Debug, Default)]
pub struct ShopCatalog { pub items: Vec<ShopItem> }

#[derive(Clone, Debug, PartialEq)]
pub enum Resolve<'a> { One(&'a ShopItem), Many(Vec<&'a ShopItem>), None }

impl ShopCatalog {
    /// Builds the catalog from the cosmetic definitions and the display assets that were found (`art`: display asset name -> `TileImage` texture path).
    pub fn build(cosmetics: &Catalog, art: &HashMap<String, Option<String>>) -> ShopCatalog {
        let by_item: HashMap<&str, (&str, bool)> = DISPLAY_ASSET_FOR_ITEM.iter().map(|(i, d, f)| (*i, (*d, *f))).collect();
        let mut items = vec![];
        for c in &cosmetics.items {
            if c.test_asset || slot_of(c.category).is_none() { continue; }
            let rarity = rarity_index(&c.rarity);
            let (tile, featured) = match by_item.get(c.id.as_str()) {
                Some((da, f)) => match art.get(*da) { Some(t) => (t.clone(), *f && t.is_some()), None => (None, false) },
                None => (None, false),
            };
            items.push(ShopItem {
                id: c.id.clone(), package: c.package.clone(), name: c.display_name().trim().to_owned(),
                short_description: c.short_description.clone().unwrap_or_else(|| c.category.name().to_owned()).trim().to_owned(),
                description: c.description.clone().unwrap_or_default().trim().to_owned(),
                category: c.category, rarity, rarity_name: c.rarity.clone(), price: item_price(&c.id, c.category, rarity),
                display_tile: tile, large_icon: c.large_icon.clone(), small_icon: c.small_icon.clone(), featured, sellable: is_shop_good(&c.id),
            });
        }
        items.sort_by(|a, b| a.id.cmp(&b.id));
        ShopCatalog { items }
    }

    pub fn get(&self, id: &str) -> Option<&ShopItem> { self.items.iter().find(|i| i.id == id) }
    pub fn by_offer(&self, offer_id: &str) -> Option<&ShopItem> { offer_id.strip_prefix("offer:").and_then(|id| self.get(id)) }
    pub fn sellable(&self) -> impl Iterator<Item = &ShopItem> { self.items.iter().filter(|i| i.sellable) }
    /// The default items every fresh profile owns, and the default loadout (first default of each slot; the male default Recruit `CID_005` for the outfit).
    pub fn default_grants(&self) -> Vec<(Slot, String)> {
        let mut v: Vec<(Slot, String)> = vec![];
        for s in Slot::ALL {
            let mut ids: Vec<&ShopItem> = self.items.iter().filter(|i| i.slot() == Some(s) && i.id.to_ascii_lowercase().contains("default")).collect();
            // the lobby has always used CID_005 as the default pawn: put it first
            ids.sort_by_key(|i| (i.id != "CID_005_Athena_Commando_M_Default", i.id.clone()));
            for i in ids { v.push((s, i.id.clone())); }
        }
        v
    }

    /// Resolve what a console user typed: exact id (any case), exact display name, id prefix, substring of id or name. A name shared by several items
    /// (e.g. "The Umbrella", "Recruit") yields `Many`.
    pub fn resolve(&self, query: &str) -> Resolve<'_> {
        let q = query.trim().trim_matches('"').to_ascii_lowercase();
        if q.is_empty() { return Resolve::None; }
        let norm = |s: &str| s.to_ascii_lowercase().replace(['_', '-', ' ', '\''], "");
        let nq = norm(&q);
        let tiers: [Box<dyn Fn(&ShopItem) -> bool>; 5] = [
            Box::new(|i| i.id.to_ascii_lowercase() == q),
            Box::new(|i| i.name.to_ascii_lowercase() == q),
            Box::new(|i| i.id.to_ascii_lowercase().starts_with(&q)),
            Box::new(|i| norm(&i.name).starts_with(&nq)),
            Box::new(|i| norm(&i.id).contains(&nq) || norm(&i.name).contains(&nq)),
        ];
        for t in tiers.iter() {
            let hits: Vec<&ShopItem> = self.items.iter().filter(|i| t(i)).collect();
            match hits.len() { 0 => continue, 1 => return Resolve::One(hits[0]), _ => return Resolve::Many(hits) }
        }
        Resolve::None
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------------------------
// rotation

/// SplitMix64 (public domain): a tiny deterministic generator, so a given UTC day always yields the same shop on every machine.
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
fn shuffled(mut v: Vec<usize>, seed: u64) -> Vec<usize> {
    let mut s = seed;
    for i in (1..v.len()).rev() { let j = (splitmix(&mut s) % (i as u64 + 1)) as usize; v.swap(i, j); }
    v
}

pub const SECS_PER_DAY: i64 = 86_400;
pub fn now_unix() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) }
/// Days since 1970-01-01 (UTC); the daily reset is the change of this number at 00:00 UTC.
pub fn utc_day(unix: i64) -> i64 { unix.div_euclid(SECS_PER_DAY) }
/// Weeks start on Thursday 00:00 UTC (Unix day 0 was a Thursday).
pub fn week_of_day(day: i64) -> i64 { day.div_euclid(7) }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rotation {
    /// the (offset-shifted) UTC day this rotation is for
    pub day: i64,
    pub week: i64,
    /// indices into [`ShopCatalog::items`]
    pub weekly: Vec<usize>,
    pub daily: Vec<usize>,
}

/// The deterministic rotation of a UTC day (reference: a 1.8 storefront). WEEKLY = `WEEKLY_SLOTS` (2) OUTFITS seeded by the week (featured outfits first, then any
/// other sellable outfit); DAILY = `DAILY_SLOTS` (4) mixed items seeded by the UTC day: one outfit, one harvesting tool, one glider, then one more of any type.
pub fn rotation(cat: &ShopCatalog, day: i64) -> Rotation {
    let week = week_of_day(day);
    let outfits_f: Vec<usize> = cat.items.iter().enumerate().filter(|(_, i)| i.sellable && i.featured && i.category == Category::Outfit).map(|(k, _)| k).collect();
    let outfits_o: Vec<usize> = cat.items.iter().enumerate().filter(|(_, i)| i.sellable && !i.featured && i.category == Category::Outfit).map(|(k, _)| k).collect();
    let wseed = (week as u64).wrapping_mul(0xA24B_AED4_963E_E407) ^ 0x0057_4545_4B4C_5900; // "WEEKLY"
    let mut weekly: Vec<usize> = shuffled(outfits_f, wseed).into_iter().take(WEEKLY_SLOTS).collect();
    if weekly.len() < WEEKLY_SLOTS { for k in shuffled(outfits_o, wseed) { if weekly.len() >= WEEKLY_SLOTS { break; } weekly.push(k); } }
    let dseed = (day as u64).wrapping_mul(0x9FB2_1C65_1E98_DF25) ^ 0x0044_4149_4C59_0000; // "DAILY"
    let mut daily: Vec<usize> = vec![];
    for (n, c) in [Some(Category::Outfit), Some(Category::Pickaxe), Some(Category::Glider), None].into_iter().enumerate() {
        let pool: Vec<usize> = cat.items.iter().enumerate().filter(|(k, i)| i.sellable && !weekly.contains(k) && !daily.contains(k) && c.map_or(true, |c| i.category == c)).map(|(k, _)| k).collect();
        if let Some(&k) = shuffled(pool, dseed.wrapping_add(n as u64 * 0x1000_0000_0001)).first() { daily.push(k); }
    }
    // the same four tiles in a seeded order
    let daily = shuffled(daily, dseed ^ 0x5348_5546);
    Rotation { day, week, weekly, daily }
}
// ---------------------------------------------------------------------------------------------------------------------------------------------------
// shop = catalog + purchase glue

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuyError { NotOnSale, Purchase(PurchaseError) }
impl std::fmt::Display for BuyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { BuyError::NotOnSale => write!(f, "that offer is not in today's rotation"), BuyError::Purchase(e) => write!(f, "{e}") }
    }
}

pub struct Shop { pub catalog: ShopCatalog }

impl Shop {
    pub fn new(catalog: ShopCatalog) -> Shop { Shop { catalog } }

    /// Reads the cosmetic definitions and the offer display assets from the install (a few hundred ms in release builds).
    pub fn load(store: &fn_ui::screens::athena_lobby::store::Store) -> Shop {
        let cosmetics = Catalog::load(store.pak_set());
        let mut art = HashMap::new();
        for (_, da, _) in DISPLAY_ASSET_FOR_ITEM.iter() {
            if let Some(a) = fn_ui::screens::athena_shop::art::display_asset(store, da) { art.insert((*da).to_owned(), a.tile_image); }
        }
        Shop::new(ShopCatalog::build(&cosmetics, &art))
    }

    /// Every sellable item as an offer (the preload draws every item tile and details art once so nothing loads lazily when a rotation brings it).
    pub fn all_offer_views(&self, profile: &Profile) -> Vec<OfferView> { self.catalog.sellable().map(|i| self.offer_view(i, profile)).collect() }

    /// The (offset-shifted) UTC day the shop shows for this profile at `now_unix`.
    pub fn day(&self, profile: &Profile, now_unix: i64) -> i64 { utc_day(now_unix) + profile.shop_day_offset }
    pub fn rotation(&self, profile: &Profile, now_unix: i64) -> Rotation { rotation(&self.catalog, self.day(profile, now_unix)) }

    fn offer_view(&self, item: &ShopItem, profile: &Profile) -> OfferView {
        let attrs = vec![(item.category.name().to_owned(), "1".to_owned())];
        OfferView {
            offer_id: item.offer_id(), name: item.name.clone(), short_description: item.short_description.clone(), description: item.description.clone(),
            rarity: item.rarity, price: item.price, owned: profile.owns(&item.id),
            tile_image: item.display_tile.clone().or_else(|| item.large_icon.clone()).or_else(|| item.small_icon.clone()),
            icon: item.large_icon.clone().or_else(|| item.small_icon.clone()),
            attributes: attrs, is_glider: item.category == Category::Glider, is_outfit: item.category == Category::Outfit,
        }
    }

    /// The two storefronts the screen shows now, with the remaining time of each (real clock: the offset only changes the seed).
    pub fn view(&self, profile: &Profile, now_unix: i64) -> ShopView {
        let rot = self.rotation(profile, now_unix);
        let into_day = now_unix.rem_euclid(SECS_PER_DAY);
        let day_ends_in = SECS_PER_DAY - into_day;
        let week_ends_in = (6 - rot.day.rem_euclid(7)) * SECS_PER_DAY + day_ends_in;
        ShopView {
            weekly: rot.weekly.iter().map(|&k| self.offer_view(&self.catalog.items[k], profile)).collect(),
            daily: rot.daily.iter().map(|&k| self.offer_view(&self.catalog.items[k], profile)).collect(),
            week_ends_in, day_ends_in,
        }
    }

    /// Buys an offer of the current rotation with V-Bucks and auto-equips it (`AthenaDirectAcquisitionOfferDetailsWidget::HandlePurchaseOfferComplete` calls
    /// `AutoEquip()` after pushing the purchased widget; `AutoEquip` is native, its effect is read as "equip the bought item into its Locker slot").
    pub fn buy(&self, profile: &mut Profile, offer_id: &str, now_unix: i64) -> Result<Receipt, BuyError> {
        let rot = self.rotation(profile, now_unix);
        let item = self.catalog.by_offer(offer_id).ok_or(BuyError::NotOnSale)?;
        let idx = self.catalog.items.iter().position(|i| i.id == item.id).ok_or(BuyError::NotOnSale)?;
        if !rot.weekly.contains(&idx) && !rot.daily.contains(&idx) { return Err(BuyError::NotOnSale); }
        let receipt = profile.purchase(&PurchaseRequest { offer_id: offer_id.to_owned(), item_id: item.id.clone(), price: item.price }).map_err(BuyError::Purchase)?;
        if let Some(slot) = item.slot() { let _ = profile.equip(slot, &item.id); }
        let _ = profile.flush();
        Ok(receipt)
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------------------------
// the screen controller (what the 1.8 Blueprints do on click / back, on top of the rendered widgets)

/// Result of a pointer / key event on the shop screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShopEvent {
    None,
    /// a tab button of the tab list was clicked (the caller switches the page; the shop is `item_shop_tab`)
    TabSelected(usize),
    /// `AttemptPurchase(0)` succeeded: the purchased widget is up, the item was auto-equipped; flush the profile and refresh the Locker / lobby pawn
    Bought { offer_id: String, item_id: String, price: i64, balance: i64 },
    /// text for the caller to show (e.g. the offline replacement of the "Get V-bucks" store)
    Message(String),
}

pub struct ShopScreen {
    pub shop: Shop,
    pub ui: fn_ui::screens::athena_shop::AthenaShopUi,
    pub state: fn_ui::screens::athena_shop::ShopState,
    hits: fn_ui::screens::athena_shop::HitMap,
}

impl ShopScreen {
    /// Loads the cosmetic catalog and composes the widgets (a few hundred ms; do it once).
    pub fn load(store: std::rc::Rc<fn_ui::screens::athena_lobby::store::Store>) -> Result<ShopScreen, String> {
        let shop = Shop::load(&store);
        Self::from_parts(shop, store)
    }
    pub fn from_parts(shop: Shop, store: std::rc::Rc<fn_ui::screens::athena_lobby::store::Store>) -> Result<ShopScreen, String> {
        let ui = fn_ui::screens::athena_shop::AthenaShopUi::from_store(store)?;
        let state = fn_ui::screens::athena_shop::ShopState { selected_tab: ui.item_shop_tab, ..Default::default() };
        Ok(ShopScreen { shop, ui, state, hits: Default::default() })
    }
    /// The items a fresh profile owns / equips (give to `Profile::load_or_new`).
    pub fn default_grants(&self) -> Vec<(Slot, String)> { self.shop.catalog.default_grants() }

    /// Entering the tab: back to the storefront.
    pub fn open(&mut self) { self.state.dialog = fn_ui::screens::athena_shop::Dialog::None; self.state.hovered_offer = None; }
    pub fn dialog_open(&self) -> bool { !matches!(self.state.dialog, fn_ui::screens::athena_shop::Dialog::None) }

    /// Rebuilds the view from the profile (balance, owned flags, the rotation of `now`) and draws. `uic` is the lobby UI's `UiContext`.
    pub fn draw(&mut self, uic: &mut fn_ui::scene::UiContext, w: f32, h: f32, profile: &Profile, now: i64) -> fn_ui::screens::athena_shop::ShopFrame {
        use fn_ui::screens::athena_shop::Dialog;
        self.state.mtx_balance = profile.balance();
        self.state.view = self.shop.view(profile, now);
        // the pushed widgets keep showing their offer (refreshed: ownership / price changes made through the console)
        match &mut self.state.dialog {
            Dialog::Details { offer, .. } | Dialog::Purchased { offer } => { if let Some(o) = self.state.view.find(&offer.offer_id) { *offer = o.clone(); } }
            Dialog::None => {}
        }
        let frame = self.ui.draw(uic, w, h, &self.state);
        self.hits = frame.hit_map();
        frame
    }

    pub fn pointer_move(&mut self, x: f32, y: f32) {
        use fn_ui::screens::athena_shop::{Dialog, ShopHit};
        let hit = self.hits.hit(x, y);
        self.state.hovered_tab = if let ShopHit::Tab(i) = hit { Some(i) } else { None };
        self.state.hovered_offer = if let ShopHit::Offer(id) = &hit { Some(id.clone()) } else { None };
        self.state.back_hovered = hit == ShopHit::Back;
        if let Dialog::Details { purchase_hovered, .. } = &mut self.state.dialog { *purchase_hovered = hit == ShopHit::Purchase; }
    }

    pub fn click(&mut self, x: f32, y: f32, profile: &mut Profile, now: i64) -> ShopEvent {
        use fn_ui::screens::athena_shop::{Dialog, ShopHit};
        match self.hits.hit(x, y) {
            ShopHit::Tab(i) => ShopEvent::TabSelected(i),
            ShopHit::Offer(id) => {
                // AthenaDirectAcquisitionOfferWidget::OnClicked: Create(OfferDetailsWidget) -> SetupOffer -> PushContentWidgetAdvanced
                if let Some(o) = self.state.view.find(&id) { self.state.dialog = Dialog::Details { offer: o.clone(), purchase_hovered: false }; }
                self.state.hovered_offer = None;
                ShopEvent::None
            }
            ShopHit::Back => { self.back(); ShopEvent::None }
            ShopHit::Purchase => self.purchase(profile, now),
            ShopHit::None => ShopEvent::None,
        }
    }

    fn purchase(&mut self, profile: &mut Profile, now: i64) -> ShopEvent {
        use fn_ui::screens::athena_shop::{Affordability, Dialog};
        let Dialog::Details { offer, .. } = &self.state.dialog else { return ShopEvent::None };
        let offer = offer.clone();
        match Affordability::of(&offer, profile.balance()) {
            Affordability::Owned => ShopEvent::None,
            // the button reads "Get V-bucks" when the V-Bucks price cannot be paid (SetupPrice: IsPriceInMTX): offline there is no store
            Affordability::CannotAfford => ShopEvent::Message(format!("Not enough V-Bucks ({} needed, {} available). There is no V-Bucks store offline: use the ~ command bar (addvbucks <n>).", offer.price, profile.balance())),
            Affordability::Can => match self.shop.buy(profile, &offer.offer_id, now) {
                Ok(r) => {
                    // HandlePurchaseOfferComplete(true): push the purchased widget, AutoEquip
                    let view = self.shop.view(profile, now);
                    let bought = view.find(&offer.offer_id).cloned().map(|mut o| { o.owned = true; o }).unwrap_or(offer);
                    self.state.view = view;
                    self.state.dialog = Dialog::Purchased { offer: bought };
                    ShopEvent::Bought { offer_id: r.offer_id, item_id: r.item_id, price: r.price, balance: r.balance_after }
                }
                Err(e) => ShopEvent::Message(format!("Purchase failed: {e}")),
            },
        }
    }

    /// Enter / controller "accept" on the details widget = pressing the (focused) purchase button.
    pub fn confirm(&mut self, profile: &mut Profile, now: i64) -> ShopEvent {
        if matches!(self.state.dialog, fn_ui::screens::athena_shop::Dialog::Details { .. }) { self.purchase(profile, now) } else { ShopEvent::None }
    }

    /// The input action `Back` (Esc / the bottom bar button): `HandleBack` -> `PopDetails` / `PopScreen` (the purchased widget pops itself and the details below it).
    /// Returns false when no pushed widget was up (the caller may then handle Esc itself).
    pub fn back(&mut self) -> bool {
        if self.dialog_open() { self.state.dialog = fn_ui::screens::athena_shop::Dialog::None; self.state.hovered_offer = None; true } else { false }
    }
}
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use fn_pose::cosmetics::Cosmetic;

    fn cos(id: &str, cat: Category, rarity: &str) -> Cosmetic {
        Cosmetic { package: format!("/Game/Athena/Items/Cosmetics/{id}"), id: id.into(), class: "x".into(), category: cat, name: Some(format!("Name {id}")), description: None, short_description: None,
            rarity: rarity.into(), test_asset: false, hero: None, hero_package: None, weapon: None, skeletal_mesh: None, small_icon: Some(format!("/Game/T/{id}.{id}")), large_icon: Some(format!("/Game/T/{id}.{id}")), animation: None }
    }
    pub fn synthetic() -> ShopCatalog {
        let mut c = Catalog::default();
        c.items.push(cos("CID_005_Athena_Commando_M_Default", Category::Outfit, "Handmade"));
        c.items.push(cos("DefaultPickaxe", Category::Pickaxe, "Handmade"));
        c.items.push(cos("DefaultGlider", Category::Glider, "Handmade"));
        c.items.push(cos("Umbrella_Gold", Category::Glider, "Handmade"));
        for n in 0..40 { c.items.push(cos(&format!("Item_{n:02}"), [Category::Outfit, Category::Pickaxe, Category::Glider][n % 3], ["Handmade", "Sturdy", "Quality", "Fine"][n % 4])); }
        let mut art = HashMap::new();
        for (item, da, _) in DISPLAY_ASSET_FOR_ITEM.iter() { let _ = item; art.insert((*da).to_owned(), Some(format!("/Game/UI/{da}.{da}"))); }
        // make a few synthetic items featured through the real mapping ids
        c.items.push(cos("Glider_Voyager", Category::Glider, "Sturdy"));
        c.items.push(cos("HalloweenScythe", Category::Pickaxe, "Sturdy"));
        c.items.push(cos("CID_030_Athena_Commando_M_Halloween", Category::Outfit, "Quality"));
        ShopCatalog::build(&c, &art)
    }

    #[test]
    fn price_table_covers_every_tier_and_is_monotonic() {
        let mut last = 0;
        for (name, idx, p, ..) in RARITY_PRICES { assert_eq!(rarity_index(name), idx); assert!(p > last); last = p; }
        assert_eq!(item_price("x", Category::Outfit, 3), 1500);
        // the reference storefront
        assert_eq!(item_price("Renegade", Category::Outfit, 1), 800);
        assert_eq!(item_price("Reaper", Category::Pickaxe, 2), 800);
        assert_eq!(item_price("Lucky", Category::Pickaxe, 1), 500);
        assert_eq!(item_price("Petunia", Category::Glider, 1), 500);
        assert_eq!(item_price("CID_030_Athena_Commando_M_Halloween", Category::Outfit, 3), 1200);
        assert_eq!(rarity_index("Quality"), 3);
    }

    #[test]
    fn catalog_excludes_defaults_and_rewards_from_sale() {
        let cat = synthetic();
        assert!(!cat.get("DefaultGlider").unwrap().sellable);
        assert!(!cat.get("Umbrella_Gold").unwrap().sellable);
        assert!(cat.get("Item_00").unwrap().sellable);
        assert_eq!(cat.get("Item_02").unwrap().price, 1200, "Quality = Epic glider = 1200");
        let d = cat.default_grants();
        assert!(d.contains(&(Slot::Outfit, "CID_005_Athena_Commando_M_Default".into())) && d.contains(&(Slot::Pickaxe, "DefaultPickaxe".into())));
        assert_eq!(cat.get("Glider_Voyager").unwrap().featured, true);
        assert!(cat.get("Glider_Voyager").unwrap().display_tile.is_some());
    }

    #[test]
    fn rotation_is_deterministic_per_day_and_changes_at_the_reset() {
        let cat = synthetic();
        let a = rotation(&cat, 20_000);
        assert_eq!(a, rotation(&cat, 20_000), "same day, same shop");
        assert_eq!((a.weekly.len(), a.daily.len()), (WEEKLY_SLOTS, DAILY_SLOTS));
        // no duplicates, nothing unsellable, weekly and daily disjoint
        let all: Vec<usize> = a.weekly.iter().chain(a.daily.iter()).copied().collect();
        let mut s = all.clone(); s.sort(); s.dedup();
        assert_eq!(s.len(), all.len());
        assert!(all.iter().all(|&k| cat.items[k].sellable));
        // the next UTC day has another daily storefront
        let b = rotation(&cat, 20_001);
        assert_ne!(a.daily, b.daily, "daily rotates at 00:00 UTC");
        // the weekly one stays for the whole week and changes at the Thursday boundary
        let week_start = 20_000 - 20_000i64.rem_euclid(7);
        let w0 = rotation(&cat, week_start).weekly;
        for d in 0..7 { assert_eq!(rotation(&cat, week_start + d).weekly, w0, "weekly constant inside a week (day {d})"); }
        let changed = (1..8).any(|k| rotation(&cat, week_start + 7 * k).weekly != w0);
        assert!(changed, "weekly changes across weeks");
    }

    #[test]
    fn rotation_boundary_is_midnight_utc() {
        assert_eq!(utc_day(0), 0);
        assert_eq!(utc_day(86_399), 0);
        assert_eq!(utc_day(86_400), 1);
        assert_eq!(utc_day(-1), -1);
        // Unix day 0 (1970-01-01) was a Thursday = the first day of a week
        assert_eq!(week_of_day(0), 0);
        assert_eq!(week_of_day(6), 0);
        assert_eq!(week_of_day(7), 1);
    }

    #[test]
    fn view_reports_countdowns_and_ownership() {
        let shop = Shop::new(synthetic());
        let mut p = Profile::new(shop.catalog.default_grants());
        let now = 20_000 * SECS_PER_DAY + 3_600;
        let v = shop.view(&p, now);
        assert_eq!(v.day_ends_in, SECS_PER_DAY - 3_600);
        assert!(v.week_ends_in >= v.day_ends_in && v.week_ends_in <= 7 * SECS_PER_DAY);
        let first = v.daily[0].clone();
        assert!(!first.owned);
        p.grant(first.offer_id.strip_prefix("offer:").unwrap());
        assert!(shop.view(&p, now).find(&first.offer_id).unwrap().owned);
    }

    #[test]
    fn rotateshop_offset_changes_the_displayed_day() {
        let shop = Shop::new(synthetic());
        let mut p = Profile::new(shop.catalog.default_grants());
        let now = 20_000 * SECS_PER_DAY + 10;
        let v0 = shop.view(&p, now);
        p.shop_day_offset = 1;
        let v1 = shop.view(&p, now);
        assert_ne!(v0.daily, v1.daily);
        assert_eq!(v1.daily, shop.view(&Profile::new(vec![]), now + SECS_PER_DAY).daily, "offset +1 day == tomorrow's rotation");
    }

    #[test]
    fn buying_deducts_grants_equips_and_refuses_what_is_not_on_sale() {
        let shop = Shop::new(synthetic());
        let mut p = Profile::new(shop.catalog.default_grants());
        let now = 20_000 * SECS_PER_DAY;
        let v = shop.view(&p, now);
        let offer = v.daily[0].clone();
        let id = offer.offer_id.strip_prefix("offer:").unwrap().to_owned();
        let r = shop.buy(&mut p, &offer.offer_id, now).unwrap();
        assert_eq!(r.price, offer.price);
        assert_eq!(p.balance(), 1_000_000 - offer.price);
        assert!(p.owns(&id));
        let slot = shop.catalog.get(&id).unwrap().slot().unwrap();
        assert_eq!(p.equipped(slot), Some(id.as_str()), "AutoEquip");
        assert_eq!(shop.buy(&mut p, &offer.offer_id, now), Err(BuyError::Purchase(PurchaseError::AlreadyOwned)));
        // an item that is not in today's rotation cannot be bought
        let off_sale = shop.catalog.items.iter().find(|i| i.sellable && !v.weekly.iter().chain(v.daily.iter()).any(|o| o.offer_id == i.offer_id())).unwrap().offer_id();
        assert_eq!(shop.buy(&mut p, &off_sale, now), Err(BuyError::NotOnSale));
        // insufficient funds
        p.set_vbucks(10);
        let other = v.daily[1].clone();
        let e = shop.buy(&mut p, &other.offer_id, now);
        assert_eq!(e, Err(BuyError::Purchase(PurchaseError::InsufficientFunds { need: other.price, have: 10 })));
        assert_eq!(p.balance(), 10);
    }

    #[test]
    fn resolve_by_id_name_and_prefix() {
        let cat = synthetic();
        assert!(matches!(cat.resolve("item_05"), Resolve::One(i) if i.id == "Item_05"));
        assert!(matches!(cat.resolve("Name Item_07"), Resolve::One(i) if i.id == "Item_07"));
        assert!(matches!(cat.resolve("Item_1"), Resolve::Many(v) if v.len() == 10));
        assert!(matches!(cat.resolve("zzz"), Resolve::None));
        assert!(matches!(cat.resolve("voyager"), Resolve::One(i) if i.id == "Glider_Voyager"));
    }

    // ---- against the real 1.8 paks (skipped when the install is not present) ----
    fn real() -> Option<(fn_ui::screens::athena_lobby::store::Store, Shop)> {
        let install = fn_ui::pak_assets::default_install();
        let store = fn_ui::screens::athena_lobby::store::Store::open(&install).ok()?;
        let shop = Shop::load(&store);
        Some((store, shop))
    }

    #[test]
    fn real_catalog_is_battle_royale_cosmetics_only_and_the_display_assets_resolve() {
        let Some((store, shop)) = real() else { eprintln!("1.8 install not found: skipped"); return };
        let cat = &shop.catalog;
        assert!(cat.items.len() > 50, "{}", cat.items.len());
        assert!(cat.items.iter().all(|i| i.package.starts_with("/Game/Athena/Items/Cosmetics/")), "only Battle Royale (Athena) cosmetics");
        for (item, da, _) in DISPLAY_ASSET_FOR_ITEM.iter() {
            let it = cat.get(item).unwrap_or_else(|| panic!("mapped item {item} missing from the paks"));
            assert!(it.display_tile.is_some(), "{item}: display asset {da} did not resolve");
        }
        assert!(cat.items.iter().filter(|i| i.sellable).count() >= WEEKLY_SLOTS + DAILY_SLOTS + 10);
        let d = cat.default_grants();
        assert!(d.iter().any(|(s, i)| *s == Slot::Outfit && i == "CID_005_Athena_Commando_M_Default"));
        assert!(d.iter().any(|(s, i)| *s == Slot::Pickaxe && i == "DefaultPickaxe") && d.iter().any(|(s, i)| *s == Slot::Glider && i == "DefaultGlider"));
        let _ = store;
        // every rotation of a month is full
        for day in 20_000..20_030 { let r = rotation(cat, day); assert_eq!((r.weekly.len(), r.daily.len()), (WEEKLY_SLOTS, DAILY_SLOTS)); assert!(r.weekly.iter().all(|&k| cat.items[k].category == Category::Outfit)); }
    }

    #[test]
    fn screen_flow_click_tile_purchase_back_with_the_real_widgets() {
        let install = fn_ui::pak_assets::default_install();
        let Ok(store) = fn_ui::screens::athena_lobby::store::Store::open(&install) else { eprintln!("1.8 install not found: skipped"); return };
        let store = std::rc::Rc::new(store);
        let mut uic = fn_ui::scene::UiContext::from_install(&install, None).expect("ui context");
        let mut screen = ShopScreen::load(store).expect("shop screen");
        let mut profile = Profile::new(screen.default_grants());
        let now = 20_000 * SECS_PER_DAY + 7_200;
        let (w, h) = (1280.0, 720.0);
        let frame = screen.draw(&mut uic, w, h, &profile, now);
        assert!(frame.missing.is_empty(), "{:?}", frame.missing);
        assert_eq!(frame.offers.len(), WEEKLY_SLOTS + DAILY_SLOTS, "every storefront tile is on screen");
        // the tab list is the 1.8 one and the shop tab is selected
        assert_eq!(frame.tabs.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["LOBBY", "LOCKER", "ITEM SHOP", "SEASON SHOP", "STATS", "LEADERBOARDS"]);
        assert_eq!(screen.ui.item_shop_tab, 2);
        // weekly tiles are the large ones, daily the small ones (SBTileSize 384x608 / 256x292 at the layout scale). An offer rect is the whole tile
        // button (its hit area), not SBTileSize: AthenaDirectAcquisitionOfferWidget is a UCommonButton whose widget-tree root is SBTileSize, and the
        // button's SButton pads its content by the style's ButtonPadding (ButtonStyle-Solid-SquareM: 8 on every side; 1.8 exe 0x14392f340 copies
        // UCommonButtonStyle+0x4b0 into NormalStyle.NormalPadding / PressedPadding, UE4.16 SButton::GetCombinedPadding), so tile = SBTileSize + 16.
        // Same rule as fn-ui `athena_shop::tests::tiles_use_the_sizes_and_the_grid_order_of_the_blueprints`.
        const BUTTON_PAD: f32 = 8.0 + 8.0;
        let scale = fn_ui::layout::dpi_scale(w, h);
        let (big, small) = (frame.offers[0].1, frame.offers[WEEKLY_SLOTS].1);
        assert!((big.w / scale - (384.0 + BUTTON_PAD)).abs() < 1.5 && (big.h / scale - (608.0 + BUTTON_PAD)).abs() < 1.5, "large tile {:?}", big);
        assert!((small.w / scale - (256.0 + BUTTON_PAD)).abs() < 1.5 && (small.h / scale - (292.0 + BUTTON_PAD)).abs() < 1.5, "small tile {:?}", small);
        // the daily grid is 2 columns x 2 rows, filled column-major (GenerateOfferWidget: row = (n-1) % 2, column = (n-1) / 2)
        let d: Vec<_> = frame.offers[WEEKLY_SLOTS..].iter().map(|(_, r)| *r).collect();
        assert!(d[0].x == d[1].x && d[0].y < d[1].y && d[2].x > d[0].x && d[2].y == d[0].y, "{d:?}");
        // click the first daily tile: the details widget is pushed
        let (id, r) = frame.offers[WEEKLY_SLOTS].clone();
        screen.pointer_move(r.x + r.w * 0.5, r.y + r.h * 0.5);
        assert_eq!(screen.state.hovered_offer.as_deref(), Some(id.as_str()));
        assert_eq!(screen.click(r.x + r.w * 0.5, r.y + r.h * 0.5, &mut profile, now), ShopEvent::None);
        assert!(screen.dialog_open());
        let frame = screen.draw(&mut uic, w, h, &profile, now);
        assert!(frame.missing.is_empty(), "{:?}", frame.missing);
        let buy = frame.purchase_button.expect("PurchaseButton1 of the details widget");
        assert!(frame.back_button.is_some(), "the bottom bar Back action is shown while a widget is pushed");
        assert!(frame.offers.is_empty(), "the storefront is covered");
        // purchase
        let price = screen.state.view.find(&id).unwrap().price;
        let ev = screen.click(buy.x + buy.w * 0.5, buy.y + buy.h * 0.5, &mut profile, now);
        assert!(matches!(ev, ShopEvent::Bought { price: p, balance: b, .. } if p == price && b == 1_000_000 - price), "{ev:?}");
        assert_eq!(profile.balance(), 1_000_000 - price);
        let item = id.strip_prefix("offer:").unwrap();
        assert!(profile.owns(item));
        assert!(matches!(screen.state.dialog, fn_ui::screens::athena_shop::Dialog::Purchased { .. }));
        let frame = screen.draw(&mut uic, w, h, &profile, now);
        assert!(frame.missing.is_empty(), "{:?}", frame.missing);
        // Back pops the purchased widget and the details widget below it; the tile now shows OWNED
        assert!(screen.back());
        assert!(!screen.back());
        let frame = screen.draw(&mut uic, w, h, &profile, now);
        assert!(screen.state.view.find(&id).unwrap().owned && frame.offers.len() == WEEKLY_SLOTS + DAILY_SLOTS);
        // the same offer cannot be bought twice; a click on an owned offer's details has no purchase action
        screen.click(frame.offers[WEEKLY_SLOTS].1.x + 5.0, frame.offers[WEEKLY_SLOTS].1.y + 5.0, &mut profile, now);
        assert_eq!(screen.confirm(&mut profile, now), ShopEvent::None);
        assert_eq!(profile.balance(), 1_000_000 - price);
        // with too little money the button is the "Get V-bucks" one and nothing is charged
        screen.back();
        profile.set_vbucks(1);
        let frame = screen.draw(&mut uic, w, h, &profile, now);
        let (id2, r2) = frame.offers[WEEKLY_SLOTS + 1].clone();
        screen.click(r2.x + 5.0, r2.y + 5.0, &mut profile, now);
        let ev = screen.confirm(&mut profile, now);
        assert!(matches!(ev, ShopEvent::Message(ref m) if m.contains("V-Bucks")), "{ev:?}");
        assert_eq!(profile.balance(), 1);
        assert!(!profile.owns(id2.strip_prefix("offer:").unwrap()));
        // clicking a tab reports it
        screen.back();
        let frame = screen.draw(&mut uic, w, h, &profile, now);
        let t = frame.tabs[0].1;
        assert_eq!(screen.click(t.x + 2.0, t.y + 2.0, &mut profile, now), ShopEvent::TabSelected(0));
    }}




