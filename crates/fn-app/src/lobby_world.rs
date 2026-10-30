//! Read-only extraction of the Fortnite 1.8 front-end lobby from the cooked packages.
//!
//! Two halves, both reading only the user's local 1.8 install (never the exe at run time, never any decrypted config):
//!
//! * the **UI half** (`read_widget_tree`, `read_button_style`, ... below): the Athena lobby UMG data used by `lobby_ui` / `fortnite-lobby`;
//! * the **scene half** ([`read_lobby_scene`], see the `SCENE DATA` section): every actor of the BR lobby world that the 3D part draws
//!   (`/Game/Maps/Frontend` + the streamed `Frontend_Lobby_Halloween` level), with UE centimetres / `RelativeRotation` carried straight
//!   from the packages (component transforms are composed through `AttachParent` exactly like `FTransform::operator*`; Blueprint
//!   component templates are merged under the instance values). The GPU side (`lobby_scene.rs`) owns the UE-to-GPU transform. Nothing is
//!   hand-placed: a value is either in a package, a class constructor default recovered from the exe (`fn-exe-defaults`), or it is listed
//!   in `LobbySceneData::missing`.

use fn_ueio::props::{self, PropList, PropValue};
use fn_ueio::{Package, Reader};
use glam::{Mat4, Quat, Vec3};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::rc::Rc;

pub const HALLOWEEN_MAP: &str = "/Game/Maps/UI/Frontend_Lobby_Halloween";
/// The actual UMG asset that owns the Battle Royale lobby screen in the 1.8
/// client.  This is deliberately separate from the map: UE composes the
/// rendered level with this widget tree at runtime.
pub const ATHENA_LOBBY_WIDGET: &str = "/Game/Athena/UI/Frontend/AthenaLobby";
pub const FRONTEND_TOP_BAR_WIDGET: &str = "/Game/UI/Frontend/TopBar/TopBar";
/// The actual parent screen that owns Athena's tab row.  The lobby itself is
/// only one content panel; recreating a row beside it would be wrong.
pub const ATHENA_TABS_SCREEN: &str = "/Game/Athena/UI/Frontend/AthenaTabsScreen";

/// The serialized root placements of the three visible Athena lobby panels.
/// Values are Slate units from the cooked `OverlaySlot`s, not coordinates
/// chosen by this runtime.  Child-widget measurement and painting belongs to
/// the UMG port; this type exists so that work cannot silently fall back to a
/// hand-laid-out lobby shell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AthenaLobbyRootLayout {
    pub season_padding: [f32; 4],
    pub player_padding: [f32; 4],
    pub launch_padding: [f32; 4],
    pub player_center_bottom: bool,
    pub launch_right_bottom: bool,
}

/// A widget listed by `AthenaLobby.WidgetTree.AllWidgets`.  The child order is
/// the serialized panel-slot order, which is also UMG's paint order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AthenaLobbyWidget {
    pub name: String,
    pub class: String,
    /// Full cooked class path when UMG serializes this as a blueprint widget.
    /// Native UMG classes have no package path and remain `None`.
    pub class_path: Option<String>,
    pub children: Vec<String>,
}

/// The root-level cooked UMG tree. This is data only; a renderer must consume
/// this tree rather than replace it with a separately authored lobby layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AthenaLobbyWidgetTree {
    pub root: String,
    pub widgets: Vec<AthenaLobbyWidget>,
}

/// One entry registered by `AthenaTabsScreen.TopTabList` in the original
/// client.  The text and route are serialized in its `FortTabListRegistrationInfo`;
/// neither is authored by the Rust runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AthenaTabRegistration {
    pub id: String,
    pub label: String,
    pub hidden: bool,
    pub content_class: Option<String>,
}

/// Directly serialized visual inputs of a UMG widget. A resource is an
/// original package object path (texture or material), never a replacement
/// image authored by this runtime.
#[derive(Clone, Debug, PartialEq)]
pub struct CookedWidgetVisual {
    pub name: String,
    pub class: String,
    pub visibility: Option<String>,
    pub brush_resource: Option<String>,
    pub brush_size: Option<[f32; 2]>,
    pub style: Option<String>,
}

/// One cooked FSlateBrush from a CommonButtonStyle.  The material/texture path
/// is intentionally retained as a package object reference.  A caller must
/// evaluate that resource's actual Slate material; it must not replace the
/// brush with a colour selected in Rust.
#[derive(Clone, Debug, PartialEq)]
pub struct CookedSlateBrush {
    pub resource: String,
    pub image_size: [f32; 2],
    pub margin: [f32; 4],
    pub draw_as: String,
}

/// The subset of `UCommonButtonStyle` needed to paint an exact, static button
/// state.  Values come from its CDO, not a handwritten style table.
#[derive(Clone, Debug, PartialEq)]
pub struct CookedButtonStyle {
    pub path: String,
    pub min_size: [f32; 2],
    pub normal: CookedSlateBrush,
    pub hovered: CookedSlateBrush,
    pub pressed: CookedSlateBrush,
    pub selected: CookedSlateBrush,
    pub disabled: CookedSlateBrush,
    pub normal_text_style: Option<String>,
    pub selected_text_style: Option<String>,
}

/// A resolved cooked UMG node.  It is deliberately a tree rather than a
/// flattened draw list: Slate layout is defined by the parent panel plus the
/// child's serialized slot, and user-widget classes own another WidgetTree.
#[derive(Clone, Debug, PartialEq)]
pub struct CookedWidgetNode {
    pub name: String,
    pub class: String,
    pub class_path: Option<String>,
    pub visibility: Option<String>,
    pub padding: [f32; 4],
    pub horizontal_alignment: Option<String>,
    pub vertical_alignment: Option<String>,
    /// The serialized CommonUI style class, when this widget owns one.
    pub style: Option<String>,
    /// `FSlateChildSize` from the owning panel slot.  `None` means the slot
    /// did not serialize a child-size rule; it is not a runtime default.
    pub slot_size_rule: Option<String>,
    pub slot_size_value: Option<f32>,
    /// Direct `USizeBox` values.  They are left unset for every other class,
    /// so a renderer can distinguish "not serialized" from a zero override.
    pub width_override: Option<f32>,
    pub height_override: Option<f32>,
    pub min_desired_width: Option<f32>,
    pub min_desired_height: Option<f32>,
    pub brush_resource: Option<String>,
    pub brush_size: Option<[f32; 2]>,
    pub children: Vec<CookedWidgetNode>,
}

fn object_path(pkg: &Package, v: Option<&PropValue>) -> Option<String> {
    let PropValue::Object(index) = v? else { return None };
    let path = if *index < 0 { pkg.import_path(*index) } else { pkg.index_name(*index) };
    (!path.is_empty()).then_some(path)
}


fn read_package(set: &fn_pak::PakSet, long: &str) -> Result<Package, String> {
    let read = |ext| set.read_game(long, ext).map(|b| b.into_owned()).unwrap_or_default();
    Package::parse(&read("uasset"), &read("uexp"), &read("ubulk"))
        .map_err(|e| format!("{long}: {e}"))
}

fn export_props(pkg: &Package, index: usize) -> Result<props::PropList, String> {
    let body = pkg.export_bytes(index).map_err(|e| e.to_string())?;
    let mut reader = Reader::new(body);
    let ctx = props::Ctx { names: &pkg.names, file_version: pkg.summary.file_version };
    props::read_props(&mut reader, &ctx).map_err(|e| e.to_string())
}

fn margin(p: &props::PropList) -> [f32; 4] {
    let Some(m) = p.fields("Padding") else { return [0.; 4]; };
    [
        m.float("Left").unwrap_or(0.) as f32,
        m.float("Top").unwrap_or(0.) as f32,
        m.float("Right").unwrap_or(0.) as f32,
        m.float("Bottom").unwrap_or(0.) as f32,
    ]
}

/// Source string of a serialized UE 4.16 `FText` with Base history.  Other
/// history types deliberately remain unresolved instead of becoming made-up
/// labels.
fn ftext_source(value: Option<&PropValue>) -> Option<String> {
    let PropValue::Raw(bytes) = value? else { return None };
    let mut reader = Reader::new(bytes);
    let _flags = reader.u32().ok()?;
    if reader.u8().ok()? as i8 != 0 { return None; }
    let _namespace = reader.fstring().ok()?;
    let _key = reader.fstring().ok()?;
    reader.fstring().ok()
}

fn named_props(pkg: &Package, package: &str, name: &str) -> Result<props::PropList, String> {
    let index = pkg.exports.iter().position(|e| e.object_name == name)
        .ok_or_else(|| format!("{package}: missing widget export {name}"))?;
    export_props(pkg, index)
}

fn export_name(pkg: &Package, object: i32) -> Option<String> {
    if object <= 0 { return None; }
    pkg.exports.get(object as usize - 1).map(|e| e.object_name.clone())
}

fn object_indices(values: Option<&Vec<PropValue>>) -> Vec<usize> {
    values.into_iter().flatten().filter_map(|v| match v {
        PropValue::Object(i) if *i > 0 => Some(*i as usize - 1),
        _ => None,
    }).collect()
}

fn brush_resource(pkg: &Package, p: &props::PropList) -> (Option<String>, Option<[f32; 2]>) {
    for name in ["Brush", "IconBrush", "Background"] {
        let Some(b) = p.fields(name) else { continue; };
        let resource = b.object("ResourceObject").and_then(|i| object_path(pkg, Some(&PropValue::Object(i))));
        let size = match b.get("ImageSize") { Some(PropValue::Vec2(v)) => Some(*v), _ => None };
        if resource.is_some() || size.is_some() { return (resource, size); }
    }
    (None, None)
}

fn slate_brush(pkg: &Package, p: &props::PropList, field: &str) -> Result<CookedSlateBrush, String> {
    let brush = p.fields(field).ok_or_else(|| format!("missing {field} SlateBrush"))?;
    let resource = brush.object("ResourceObject")
        .and_then(|i| object_path(pkg, Some(&PropValue::Object(i))))
        .ok_or_else(|| format!("{field}: SlateBrush has no ResourceObject"))?;
    let image_size = match brush.get("ImageSize") {
        Some(PropValue::Vec2(value)) => *value,
        _ => return Err(format!("{field}: SlateBrush has no ImageSize")),
    };
    let margin = [
        brush.fields("Margin").and_then(|m| m.float("Left")).unwrap_or(0.) as f32,
        brush.fields("Margin").and_then(|m| m.float("Top")).unwrap_or(0.) as f32,
        brush.fields("Margin").and_then(|m| m.float("Right")).unwrap_or(0.) as f32,
        brush.fields("Margin").and_then(|m| m.float("Bottom")).unwrap_or(0.) as f32,
    ];
    let draw_as = brush.text("DrawAs").unwrap_or_default().to_owned();
    Ok(CookedSlateBrush { resource, image_size, margin, draw_as })
}

/// Reads an actual `UCommonButtonStyle` CDO from the supplied 1.8 install.
/// The `style_path` may include an object suffix (`.ButtonStyle-..._C`), which
/// is stripped because UE stores style class and CDO in its package.
pub fn read_button_style(install: &Path, style_path: &str) -> Result<CookedButtonStyle, String> {
    let package = style_path.split_once('.').map(|(package, _)| package).unwrap_or(style_path);
    let set = fn_pak::PakSet::open_install(install).map_err(|e| e.to_string())?;
    let pkg = read_package(&set, package)?;
    let cdo = pkg.exports.iter().position(|e| e.object_name.starts_with("Default__"))
        .ok_or_else(|| format!("{package}: no style CDO"))?;
    let p = export_props(&pkg, cdo)?;
    Ok(CookedButtonStyle {
        path: style_path.to_owned(),
        min_size: [p.float("MinWidth").unwrap_or(0.) as f32, p.float("MinHeight").unwrap_or(0.) as f32],
        normal: slate_brush(&pkg, &p, "NormalBase")?,
        hovered: slate_brush(&pkg, &p, "NormalHovered")?,
        pressed: slate_brush(&pkg, &p, "NormalPressed")?,
        selected: slate_brush(&pkg, &p, "SelectedBase")?,
        disabled: slate_brush(&pkg, &p, "Disabled")?,
        normal_text_style: p.object("NormalTextStyle").and_then(|i| object_path(&pkg, Some(&PropValue::Object(i)))),
        selected_text_style: p.object("SelectedTextStyle").and_then(|i| object_path(&pkg, Some(&PropValue::Object(i)))),
    })
}

/// Decodes `AthenaLobby.WidgetTree.AllWidgets` and each panel's `Slots`
/// directly from the 1.8 UMG package. No names, child order, or hierarchy are
/// duplicated as a Rust constant.
pub fn read_widget_tree(install: &Path, package: &str, tree_name: &str) -> Result<AthenaLobbyWidgetTree, String> {
    let set = fn_pak::PakSet::open_install(install).map_err(|e| e.to_string())?;
    let pkg = read_package(&set, package)?;
    let tree = named_props(&pkg, package, tree_name)?;
    let root = tree.object("RootWidget").and_then(|i| export_name(&pkg, i))
        .ok_or_else(|| format!("{package}: {tree_name} has no RootWidget"))?;
    let mut widgets = Vec::new();
    for index in object_indices(tree.array("AllWidgets")) {
        let name = pkg.exports.get(index).ok_or_else(|| format!("{package}: AllWidgets refers outside the export table"))?.object_name.clone();
        let p = export_props(&pkg, index)?;
        let mut children = Vec::new();
        for slot in object_indices(p.array("Slots")) {
            let slot_props = export_props(&pkg, slot)?;
            if let Some(child) = slot_props.object("Content").and_then(|i| export_name(&pkg, i)) {
                children.push(child);
            }
        }
        let class_index = pkg.exports[index].class_index;
        let class_path = (class_index < 0).then(|| pkg.import_path(class_index))
            .filter(|path| path.starts_with("/Game/"));
        widgets.push(AthenaLobbyWidget { name, class: pkg.export_class(index).to_owned(), class_path, children });
    }
    Ok(AthenaLobbyWidgetTree { root, widgets })
}

pub fn read_athena_lobby_widget_tree(install: &Path) -> Result<AthenaLobbyWidgetTree, String> {
    read_widget_tree(install, ATHENA_LOBBY_WIDGET, "WidgetTree")
}

/// The Athena front-end's tab host.  This is separate from AthenaLobby: it
/// serializes `TopTabList`, `ButtonStyle-Tab-Main_C`, its exact per-tab
/// padding, labels, and content classes.  The renderer must compose this host
/// around AthenaLobby rather than author an independent navigation row.
pub fn read_athena_tabs_widget_tree(install: &Path) -> Result<AthenaLobbyWidgetTree, String> {
    read_widget_tree(install, ATHENA_TABS_SCREEN, "WidgetTree")
}

/// Reads the exact registered tab list from `AthenaTabsScreen.TopTabList`.
/// This is the source of the Season 1 Athena labels, visibility, and screen
/// class routing; rendering code must consume this rather than carry a tab
/// name array.
pub fn read_athena_tab_registrations(install: &Path) -> Result<Vec<AthenaTabRegistration>, String> {
    let set = fn_pak::PakSet::open_install(install).map_err(|error| error.to_string())?;
    let pkg = read_package(&set, ATHENA_TABS_SCREEN)?;
    let p = named_props(&pkg, ATHENA_TABS_SCREEN, "TopTabList")?;
    let mut tabs = Vec::new();
    for value in p.array("PreregisteredTabInfoArray").into_iter().flatten() {
        let PropValue::Struct { fields, .. } = value else { continue };
        let Some(id) = fields.text("TabNameId") else { continue };
        let label = fields.fields("TabLabelInfo")
            .and_then(|label| ftext_source(label.get("DisplayName")))
            .unwrap_or_default();
        tabs.push(AthenaTabRegistration {
            id: id.to_owned(),
            label,
            hidden: fields.bool("bHidden").unwrap_or(false),
            content_class: fields.object("TabContentType").and_then(|object| object_path(&pkg, Some(&PropValue::Object(object)))),
        });
    }
    if tabs.is_empty() { return Err(format!("{ATHENA_TABS_SCREEN}.TopTabList has no serialized tab registrations")); }
    Ok(tabs)
}

/// Resolves the lobby's direct Blueprint-widget children from the class paths
/// serialized on the root UMG tree.  This deliberately derives package names
/// from the cooked references (`Foo.Foo_C -> Foo`) instead of carrying a
/// duplicate list of PlayerPanel/Matchmaking/Quest packages in Rust.
pub fn read_athena_lobby_child_widget_trees(install: &Path) -> Result<Vec<(String, AthenaLobbyWidgetTree)>, String> {
    let root = read_athena_lobby_widget_tree(install)?;
    root.widgets.into_iter().filter_map(|widget| {
        let class_path = widget.class_path?;
        let (package, object) = class_path.rsplit_once('.')?;
        object.ends_with("_C").then_some((widget.name, package.to_owned()))
    }).map(|(name, package)| read_widget_tree(install, &package, "WidgetTree").map(|tree| (name, tree)))
        .collect()
}

fn class_path(pkg: &Package, export: usize) -> Option<String> {
    let index = pkg.exports.get(export)?.class_index;
    (index < 0).then(|| pkg.import_path(index)).filter(|path| path.starts_with("/Game/"))
}

fn node_from_export(set: &fn_pak::PakSet, pkg: &Package, index: usize, depth: usize) -> Result<CookedWidgetNode, String> {
    if depth > 32 { return Err("UMG Blueprint nesting exceeds 32 levels".into()); }
    let export = pkg.exports.get(index).ok_or("UMG node outside export table")?;
    let p = export_props(pkg, index)?;
    let class_path = class_path(pkg, index);
    let (brush_resource, brush_size) = brush_resource(pkg, &p);
    let slot = p.object("Slot").and_then(|slot| (slot > 0).then_some(slot as usize - 1));
    let (padding, horizontal_alignment, vertical_alignment, slot_size_rule, slot_size_value) = match slot {
        Some(slot) => {
            let slot_props = export_props(pkg, slot)?;
            let (rule, value) = slot_props.fields("Size").map(|size| {
                (size.text("SizeRule").map(str::to_owned), size.float("Value").map(|value| value as f32))
            }).unwrap_or((None, None));
            (
                margin(&slot_props),
                slot_props.text("HorizontalAlignment").map(str::to_owned),
                slot_props.text("VerticalAlignment").map(str::to_owned),
                rule,
                value,
            )
        }
        None => ([0.; 4], None, None, None, None),
    };
    let mut children = Vec::new();
    for slot in object_indices(p.array("Slots")) {
        let props = export_props(pkg, slot)?;
        if let Some(content) = props.object("Content").filter(|value| *value > 0) {
            children.push(node_from_export(set, pkg, content as usize - 1, depth + 1)?);
        }
    }
    // A user-widget instance has no direct panel Slots: recursively pull the
    // child WidgetTree from its serialized BlueprintGeneratedClass package.
    if children.is_empty() {
        if let Some(path) = class_path.as_deref() {
            if let Some((package, object)) = path.rsplit_once('.') {
                if object.ends_with("_C") {
                    let child_pkg = read_package(set, package)?;
                    if let Ok(tree) = named_props(&child_pkg, package, "WidgetTree") {
                        if let Some(root) = tree.object("RootWidget").filter(|value| *value > 0) {
                            children.push(node_from_export(set, &child_pkg, root as usize - 1, depth + 1)?);
                        }
                    }
                }
            }
        }
    }
    Ok(CookedWidgetNode {
        name: export.object_name.clone(),
        class: pkg.export_class(index), class_path,
        visibility: p.text("Visibility").map(str::to_owned),
        padding, horizontal_alignment, vertical_alignment,
        // CommonButtonBase calls this `Style`; FortTabListWidgetBase calls
        // the same class reference `ButtonStyle`.
        style: p.object("Style").or_else(|| p.object("ButtonStyle"))
            .and_then(|style| object_path(pkg, Some(&PropValue::Object(style)))),
        slot_size_rule, slot_size_value,
        width_override: p.float("WidthOverride").map(|value| value as f32),
        height_override: p.float("HeightOverride").map(|value| value as f32),
        min_desired_width: p.float("MinDesiredWidth").map(|value| value as f32),
        min_desired_height: p.float("MinDesiredHeight").map(|value| value as f32),
        brush_resource, brush_size, children,
    })
}

/// Resolves the actual Athena lobby UMG tree, expanding direct user-widget
/// Blueprint instances into their own serialized WidgetTrees.  No visual
/// asset, label, padding, or ordering is reconstructed from a screenshot.
pub fn read_athena_lobby_widget_graph(install: &Path) -> Result<CookedWidgetNode, String> {
    let set = fn_pak::PakSet::open_install(install).map_err(|error| error.to_string())?;
    let pkg = read_package(&set, ATHENA_LOBBY_WIDGET)?;
    let tree = named_props(&pkg, ATHENA_LOBBY_WIDGET, "WidgetTree")?;
    let root = tree.object("RootWidget").filter(|value| *value > 0)
        .ok_or_else(|| format!("{ATHENA_LOBBY_WIDGET}: WidgetTree has no root"))?;
    node_from_export(&set, &pkg, root as usize - 1, 0)
}

/// Reads direct brush/style state for every widget in a cooked WidgetTree.
pub fn read_widget_visuals(install: &Path, package: &str, tree_name: &str) -> Result<Vec<CookedWidgetVisual>, String> {
    let set = fn_pak::PakSet::open_install(install).map_err(|e| e.to_string())?;
    let pkg = read_package(&set, package)?;
    let tree = named_props(&pkg, package, tree_name)?;
    let mut out = Vec::new();
    for index in object_indices(tree.array("AllWidgets")) {
        let export = pkg.exports.get(index).ok_or_else(|| format!("{package}: AllWidgets refers outside the export table"))?;
        let p = export_props(&pkg, index)?;
        let (brush_resource, brush_size) = brush_resource(&pkg, &p);
        out.push(CookedWidgetVisual {
            name: export.object_name.clone(),
            class: pkg.export_class(index).to_owned(),
            visibility: p.text("Visibility").map(str::to_owned),
            brush_resource,
            brush_size,
            style: p.object("Style").or_else(|| p.object("ButtonStyle"))
                .and_then(|i| object_path(&pkg, Some(&PropValue::Object(i)))),
        });
    }
    Ok(out)
}

/// Reads a widget that is owned by an inherited/archetype WidgetTree rather
/// than appearing in the outer tree's `AllWidgets` array.  This is important
/// for the Athena player panel: its `CommonBorder_0` is an archetype child,
/// but its `Background` is still the authoritative cooked visual and must not
/// be replaced by a locally selected fill colour.
pub fn read_named_widget_brush(install: &Path, package: &str, name: &str) -> Result<CookedSlateBrush, String> {
    let set = fn_pak::PakSet::open_install(install).map_err(|e| e.to_string())?;
    let pkg = read_package(&set, package)?;
    let p = named_props(&pkg, package, name)?;
    slate_brush(&pkg, &p, "Background")
}

/// Reads the serialized `USizeBox` measurements for a named widget.  Values
/// remain optional because a missing override is meaningfully different from
/// an authored zero.  This does not apply layout defaults on behalf of UE.
pub fn read_named_widget_node(install: &Path, package: &str, name: &str) -> Result<CookedWidgetNode, String> {
    let set = fn_pak::PakSet::open_install(install).map_err(|e| e.to_string())?;
    let pkg = read_package(&set, package)?;
    let index = pkg.exports.iter().position(|export| export.object_name == name)
        .ok_or_else(|| format!("{package}: missing widget export {name}"))?;
    node_from_export(&set, &pkg, index, 0)
}

/// Reads the actual root slots of `AthenaLobby.WidgetTree.OverlayMain`.
///
/// The slot names are stable serialized identities in this particular 1.8
/// package. Their contents are additionally checked so a same-named nested
/// slot can never be used by accident.
pub fn read_athena_lobby_root_layout(install: &Path) -> Result<AthenaLobbyRootLayout, String> {
    let set = fn_pak::PakSet::open_install(install).map_err(|e| e.to_string())?;
    let pkg = read_package(&set, ATHENA_LOBBY_WIDGET)?;
    let season = named_props(&pkg, ATHENA_LOBBY_WIDGET, "OverlaySlot_1")?;
    let player = named_props(&pkg, ATHENA_LOBBY_WIDGET, "OverlaySlot_6")?;
    let launch = named_props(&pkg, ATHENA_LOBBY_WIDGET, "OverlaySlot_3")?;
    let content = |p: &props::PropList| p.object("Content")
        .and_then(|i| object_path(&pkg, Some(&PropValue::Object(i))));
    let season_content = content(&season);
    if season_content.as_deref().map(|v| !v.contains("SeasonAndChallengesPanel")).unwrap_or(true) {
        return Err(format!("{ATHENA_LOBBY_WIDGET}: OverlaySlot_1 is not SeasonAndChallengesPanel ({season_content:?})"));
    }
    let player_content = content(&player);
    if player_content.as_deref().map(|v| !v.contains("PlayerPanel")).unwrap_or(true) {
        return Err(format!("{ATHENA_LOBBY_WIDGET}: OverlaySlot_6 is not PlayerPanel ({player_content:?})"));
    }
    let launch_content = content(&launch);
    if launch_content.as_deref().map(|v| !v.ends_with("Launch")).unwrap_or(true) {
        return Err(format!("{ATHENA_LOBBY_WIDGET}: OverlaySlot_3 is not Launch ({launch_content:?})"));
    }
    Ok(AthenaLobbyRootLayout {
        season_padding: margin(&season),
        player_padding: margin(&player),
        launch_padding: margin(&launch),
        player_center_bottom: player.text("HorizontalAlignment") == Some("HAlign_Center")
            && player.text("VerticalAlignment") == Some("VAlign_Bottom"),
        launch_right_bottom: launch.text("HorizontalAlignment") == Some("HAlign_Right")
            && launch.text("VerticalAlignment") == Some("VAlign_Bottom"),
    })
}

// =====================================================================================================================================
// SCENE DATA
// =====================================================================================================================================

/// The persistent front-end level. Its `FrontEndSettingsBP_31` actor maps `EFrontEndCamera::HeroSelect` (the value `AthenaLobby` selects
/// with `SetFrontEndCamera(11)`, see SCENE.md) to the `Frontend_Lobby` actor; its level blueprint streams `/Game/Maps/UI/Frontend_Lobby`.
pub const FRONTEND_MAP: &str = "/Game/Maps/Frontend";
/// The streamed level whose level blueprint (`Frontend_Lobby_C`) picks the lobby level by its `LobbySelect` enum.
pub const LOBBY_SELECTOR_MAP: &str = "/Game/Maps/UI/Frontend_Lobby";
pub const DEFAULT_LOBBY_MAP: &str = "/Game/Maps/UI/Frontend_Lobby_Default";
/// `FortniteLobby` user enum: `NewEnumerator0` = "Default", `NewEnumerator1` = "Halloween".
pub const LOBBY_ENUM_PACKAGE: &str = "/Game/Environments/FrontEnd/Blueprints/FortniteLobby";
/// `EFrontEndCamera::HeroSelect`: the byte `AthenaLobby` passes to `SetFrontEndCamera` (disassembly offset 0x174). The enumerator order is the
/// registration order of the enum's names in the exe (Invalid 0, HomeBase 1, MissionControl 2, Store 3, Vault 4, SkillTrees 5, Heroes 6,
/// Login 7, TutorialPhaseOne..Three 8..10, HeroSelect 11, Party 12, WorldMap 13, Home 14, StoreItemInspect 15, Cosmetics 16, SpecialEvent 17).
pub const FRONTEND_CAMERA_HERO_SELECT: u8 = 11;

const EXE_DEFAULTS_JSON: &str = include_str!("../../fn-exe-defaults/reports/exe-defaults.json");

/// Native class / `FPostProcessSettings` constructor defaults recovered from the 1.8 exe by `fn-exe-defaults` (static, nothing executed).
pub struct ExeDefaults {
    pps: HashMap<String, Vec<f32>>,
    classes: HashMap<String, HashMap<String, Vec<f32>>>,
}

fn json_floats(v: &serde_json::Value) -> Option<Vec<f32>> {
    match v {
        serde_json::Value::Number(n) => Some(vec![n.as_f64()? as f32]),
        serde_json::Value::Array(a) => a.iter().map(|x| x.as_f64().map(|x| x as f32)).collect(),
        _ => None,
    }
}

impl ExeDefaults {
    pub fn get() -> &'static ExeDefaults {
        static CELL: std::sync::OnceLock<ExeDefaults> = std::sync::OnceLock::new();
        CELL.get_or_init(|| {
            let root: serde_json::Value = serde_json::from_str(EXE_DEFAULTS_JSON).expect("exe-defaults.json");
            let mut pps = HashMap::new();
            if let Some(fields) = root.pointer("/post_process_settings/fields").and_then(|f| f.as_array()) {
                for f in fields {
                    if let (Some(n), Some(v)) = (f.get("name").and_then(|n| n.as_str()), f.get("value").and_then(json_floats)) { pps.insert(n.to_owned(), v); }
                }
            }
            let mut classes = HashMap::new();
            if let Some(obj) = root.as_object() {
                for (class, v) in obj {
                    let Some(defs) = v.get("defaults").and_then(|d| d.as_array()) else { continue };
                    let mut m = HashMap::new();
                    for d in defs {
                        if let (Some(n), Some(val)) = (d.get("name").and_then(|n| n.as_str()), d.get("value").and_then(json_floats)) { m.insert(n.to_owned(), val); }
                    }
                    classes.insert(class.clone(), m);
                }
            }
            ExeDefaults { pps, classes }
        })
    }
    /// `FPostProcessSettings` constructor value of `field` (floats of the field; colours as 4 floats).
    pub fn pps(&self, field: &str) -> Option<&[f32]> { self.pps.get(field).map(|v| v.as_slice()) }
    pub fn pps1(&self, field: &str) -> f32 { self.pps(field).and_then(|v| v.first().copied()).unwrap_or(0.) }
    pub fn class(&self, class: &str, field: &str) -> Option<&[f32]> { self.classes.get(class)?.get(field).map(|v| v.as_slice()) }
    pub fn class1(&self, class: &str, field: &str) -> f32 { self.class(class, field).and_then(|v| v.first().copied()).unwrap_or(0.) }
}

/// Constructor defaults of `UPointLightComponent` / `USpotLightComponent` recovered from the 1.8 exe with
/// `classdump UPointLightComponent USpotLightComponent` (the store RVAs are in SCENE.md): `Intensity` 5000 (`+0x2a4`, RVA 0x20a5410),
/// `AttenuationRadius` 1000 (`+0x3c4`), `LightFalloffExponent` 8 (`+0x3cc`), `SourceRadius` 0, `SourceLength` 0; spot: `InnerConeAngle` 0
/// (`+0x3e8`), `OuterConeAngle` 44 (`+0x3ec`, RVA 0x20f0670).
pub mod point_defaults {
    pub const INTENSITY: f32 = 5000.0;
    pub const ATTENUATION_RADIUS: f32 = 1000.0;
    pub const FALLOFF_EXPONENT: f32 = 8.0;
    pub const SOURCE_RADIUS: f32 = 0.0;
    pub const SOURCE_LENGTH: f32 = 0.0;
    pub const SPOT_INNER_CONE_DEG: f32 = 0.0;
    pub const SPOT_OUTER_CONE_DEG: f32 = 44.0;
}

/// UE `FRotator::Quaternion()` for a serialized `(Pitch, Yaw, Roll)` in degrees.
static SHADOW_QUALITY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(3);
pub fn set_shadow_quality(q: u8) { SHADOW_QUALITY.store(q, std::sync::atomic::Ordering::Relaxed); }
pub fn shadow_quality() -> u8 { SHADOW_QUALITY.load(std::sync::atomic::Ordering::Relaxed) }

pub fn rotator_quat(pyr_deg: [f32; 3]) -> Quat {
    let (pitch, yaw, roll) = (pyr_deg[0].to_radians() * 0.5, pyr_deg[1].to_radians() * 0.5, pyr_deg[2].to_radians() * 0.5);
    let (sp, cp) = pitch.sin_cos();
    let (sy, cy) = yaw.sin_cos();
    let (sr, cr) = roll.sin_cos();
    Quat::from_xyzw(
        cr * sp * sy - sr * cp * cy,
        -cr * sp * cy - sr * cp * sy,
        cr * cp * sy - sr * sp * cy,
        cr * cp * cy + sr * sp * sy,
    )
}

/// An `FTransform` in UE space (centimetres, left-handed X forward / Y right / Z up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xf { pub t: Vec3, pub q: Quat, pub s: Vec3 }
impl Xf {
    pub const IDENTITY: Xf = Xf { t: Vec3::ZERO, q: Quat::IDENTITY, s: Vec3::ONE };
    pub fn from_pyr(loc: [f32; 3], pyr: [f32; 3], scale: [f32; 3]) -> Xf { Xf { t: Vec3::from(loc), q: rotator_quat(pyr), s: Vec3::from(scale) } }
    /// `FTransform::operator*(child, parent)` (the component-to-world step): scale, then rotate, then translate by the parent.
    pub fn compose(&self, parent: &Xf) -> Xf {
        Xf { s: self.s * parent.s, q: parent.q * self.q, t: parent.q * (parent.s * self.t) + parent.t }
    }
    pub fn transform_point(&self, p: Vec3) -> Vec3 { self.q * (self.s * p) + self.t }
    pub fn matrix(&self) -> Mat4 { Mat4::from_scale_rotation_translation(self.s, self.q, self.t) }
    /// Forward (+X) axis in world space.
    pub fn forward(&self) -> Vec3 { self.q * Vec3::X }
    /// Rotation as a UE `FRotator` `(Pitch, Yaw, Roll)` in degrees (for reports).
    pub fn rotator_deg(&self) -> [f32; 3] {
        // FQuat::Rotator()
        let q = self.q;
        let singularity = q.z * q.x - q.w * q.y;
        let yaw_y = 2.0 * (q.w * q.z + q.x * q.y);
        let yaw_x = 1.0 - 2.0 * (q.y * q.y + q.z * q.z);
        let (pitch, yaw, roll);
        if singularity < -0.4999995 {
            pitch = -90.0; yaw = yaw_y.atan2(yaw_x).to_degrees();
            roll = -(yaw - 2.0 * q.x.atan2(q.w).to_degrees()).rem_euclid(360.0);
        } else if singularity > 0.4999995 {
            pitch = 90.0; yaw = yaw_y.atan2(yaw_x).to_degrees();
            roll = (yaw - 2.0 * q.x.atan2(q.w).to_degrees()).rem_euclid(360.0);
        } else {
            pitch = (2.0 * singularity).asin().to_degrees();
            yaw = yaw_y.atan2(yaw_x).to_degrees();
            roll = (-2.0 * (q.w * q.x + q.y * q.z)).atan2(1.0 - 2.0 * (q.x * q.x + q.y * q.y)).to_degrees();
        }
        [pitch, yaw, roll]
    }
}

fn pv_vec3(p: &PropList, name: &str) -> Option<[f32; 3]> { match p.get(name) { Some(PropValue::Vec3(v)) => Some(*v), _ => None } }
fn pv_vec4(p: &PropList, name: &str) -> Option<[f32; 4]> { match p.get(name) { Some(PropValue::Vec4(v)) => Some(*v), _ => None } }
/// `FColor` as `[r, g, b, a]` (the package stores B, G, R, A).
fn pv_color(p: &PropList, name: &str) -> Option<[u8; 4]> { match p.get(name) { Some(PropValue::Color(c)) => Some([c[2], c[1], c[0], c[3]]), _ => None } }
fn pv_f32(p: &PropList, name: &str) -> Option<f32> { p.float(name).map(|v| v as f32) }

/// A package with a lazily parsed property cache per export.
struct PkgView {
    long: String,
    pkg: Package,
    cache: RefCell<HashMap<usize, Rc<PropList>>>,
}
impl PkgView {
    fn open(set: &fn_pak::PakSet, long: &str, map: bool) -> Result<PkgView, String> {
        let rd = |ext: &str| set.read_game(long, ext).map(|b| b.into_owned()).unwrap_or_default();
        let header = rd(if map { "umap" } else { "uasset" });
        if header.is_empty() { return Err(format!("{long}: not in the paks")); }
        let pkg = Package::parse(&header, &rd("uexp"), &rd("ubulk")).map_err(|e| format!("{long}: {e}"))?;
        Ok(PkgView { long: long.to_owned(), pkg, cache: RefCell::new(HashMap::new()) })
    }
    fn props(&self, i: usize) -> Rc<PropList> {
        if let Some(p) = self.cache.borrow().get(&i) { return p.clone(); }
        let (start, end) = self.pkg.export_range(i);
        let ctx = props::Ctx { names: &self.pkg.names, file_version: self.pkg.summary.file_version };
        let list = if end <= self.pkg.data.len() && start <= end {
            props::read_props(&mut Reader::at(&self.pkg.data[..end], start), &ctx).unwrap_or_default()
        } else { PropList::default() };
        let rc = Rc::new(list);
        self.cache.borrow_mut().insert(i, rc.clone());
        rc
    }
    fn n(&self) -> usize { self.pkg.exports.len() }
    fn name(&self, i: usize) -> &str { &self.pkg.exports[i].object_name }
    fn class(&self, i: usize) -> String { self.pkg.export_class(i) }
    /// Full class path (`/Game/..Pkg.Class_C` / `/Script/..`) of an export.
    fn class_path(&self, i: usize) -> Option<String> {
        let ci = self.pkg.exports[i].class_index;
        if ci < 0 { Some(self.pkg.import_path(ci)).filter(|s| !s.is_empty()) } else { None }
    }
    /// Export index (0-based) of an object reference into this same package.
    fn export_of(&self, v: Option<&PropValue>) -> Option<usize> {
        match v { Some(PropValue::Object(i)) if *i > 0 && (*i as usize) <= self.n() => Some(*i as usize - 1), _ => None }
    }
    /// Object path of any reference: `Pkg.Obj` for imports, the export's name for same-package objects.
    fn path_of(&self, v: Option<&PropValue>) -> Option<String> { object_path(&self.pkg, v) }
    fn soft_or_hard_path(&self, v: Option<&PropValue>) -> Option<String> {
        match v { Some(PropValue::Str(s)) if !s.is_empty() => Some(s.clone()), _ => self.path_of(v) }
    }
    fn owner(&self, i: usize) -> Option<usize> { let o = self.pkg.exports[i].outer_index; (o > 0).then(|| o as usize - 1) }
    fn children(&self, owner: usize) -> Vec<usize> {
        (0..self.n()).filter(|&i| self.pkg.exports[i].outer_index == owner as i32 + 1).collect()
    }
}

/// `new` over `base`, property by property (instance values replace template values).
fn overlay(base: &mut PropList, new: &PropList) {
    for p in &new.0 {
        if let Some(b) = base.0.iter_mut().find(|b| b.name == p.name && b.array_index == p.array_index) { *b = p.clone(); } else { base.0.push(p.clone()); }
    }
}

/// A cooked Blueprint class: its component templates (`<Var>_GEN_VARIABLE`, plus the `Default__X_C` native components) and the parent chain.
struct BpClass {
    view: PkgView,
    class_export: usize,
    cdo: Option<usize>,
    parent: Option<Rc<BpClass>>,
    /// `SCS_Node.InternalVariableName` -> its `ComponentTemplate` export (templates are not always called `<Var>_GEN_VARIABLE`, e.g.
    /// `CharacterPlacement` is `StaticMeshComponent_231` and `DefaultSceneRoot` is `SceneComponent_7`).
    scs: HashMap<String, usize>,
}
impl BpClass {
    fn find_template(&self, name: &str) -> Option<usize> {
        if let Some(&t) = self.scs.get(name) { return Some(t); }
        let gen = format!("{name}_GEN_VARIABLE");
        (0..self.view.n()).find(|&i| {
            let e = &self.view.pkg.exports[i];
            (e.outer_index == self.class_export as i32 + 1 && e.object_name == gen) || self.cdo.is_some_and(|c| e.outer_index == c as i32 + 1 && e.object_name == name)
        })
    }
    /// Object references of a template point into the Blueprint's own package; the merged list must be package independent, so they become
    /// object path strings (`Pkg.Obj`), and `AttachParent` (an export of the Blueprint package) is dropped: the instance supplies it.
    fn localize(&self, p: &PropList) -> PropList {
        PropList(p.0.iter().filter(|x| x.name != "AttachParent").map(|x| { let mut x = x.clone(); x.value = self.localize_value(&x.value); x }).collect())
    }
    fn localize_value(&self, v: &PropValue) -> PropValue {
        match v {
            PropValue::Object(i) if *i < 0 => PropValue::Str(self.view.pkg.import_path(*i)),
            PropValue::Object(i) if *i > 0 => PropValue::Str(format!("{}:{}", self.view.long, self.view.name(*i as usize - 1))),
            PropValue::Array(a) => PropValue::Array(a.iter().map(|x| self.localize_value(x)).collect()),
            PropValue::Struct { name, fields } => PropValue::Struct { name: name.clone(), fields: self.localize(fields) },
            other => other.clone(),
        }
    }
    /// Template properties of component `name`, base class first, so a child class's override wins (`InheritableComponentHandler`).
    fn merged(&self, name: &str) -> PropList {
        let mut out = match &self.parent { Some(p) => p.merged(name), None => PropList::default() };
        if let Some(t) = self.find_template(name) { overlay(&mut out, &self.localize(&self.view.props(t))); }
        out
    }
    /// `Default__X_C` properties merged down the chain (class defaults of the actor).
    fn cdo_props(&self) -> PropList {
        let mut out = match &self.parent { Some(p) => p.cdo_props(), None => PropList::default() };
        if let Some(c) = self.cdo { overlay(&mut out, &self.localize(&self.view.props(c))); }
        out
    }
    fn chain_names(&self) -> Vec<String> {
        let mut v = vec![self.view.long.clone()];
        if let Some(p) = &self.parent { v.extend(p.chain_names()); }
        v
    }
}

/// One light of the lobby (UE units; `intensity` and colour as serialized / defaulted, see `ExeDefaults`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightKind { Directional, Point, Spot, Sky }

#[derive(Clone, Debug)]
pub struct LightInst {
    pub actor: String,
    pub component: String,
    pub kind: LightKind,
    pub xf: Xf,
    /// `bVisible` of the component after the lobby light switcher (`BP_FortniteLobbyLightSwitcher`) ran.
    pub visible: bool,
    pub affects_world: bool,
    pub intensity: f32,
    pub color_srgb: [u8; 3],
    pub use_temperature: bool,
    pub temperature: f32,
    pub attenuation_radius: f32,
    pub falloff_exponent: f32,
    pub inverse_squared: bool,
    pub source_radius: f32,
    pub source_length: f32,
    pub inner_cone_deg: f32,
    pub outer_cone_deg: f32,
    pub cast_shadows: bool,
    pub cascades: i32,
    pub shadow_distance: f32,
    pub light_source_angle: f32,
    pub volumetric_scattering: f32,
    pub indirect_intensity: f32,
    pub max_draw_distance: f32,
    pub max_distance_fade_range: f32,
    /// `Mobility` as serialized (`None` = not serialized, the native default applies), light function material / IES texture paths, `bUseIESBrightness`, lighting channels (raw), `bUseInverseSquaredFalloff` serialized?
    pub mobility: Option<String>,
    pub light_function: Option<String>,
    pub ies_texture: Option<String>,
    pub lighting_channels: Option<String>,
    pub specular_scale: f32,
    /// Sky light: the cubemap object path (`/Engine/...` or `/Game/...`).
    pub cubemap: Option<String>,
    pub cubemap_angle_deg: f32,
    pub lower_hemisphere_color: [f32; 3],
    pub occlusion_max_distance: f32,
    /// Where the component came from (`level actor`, `blueprint BP_X_C`).
    pub source: String,
}

#[derive(Clone, Debug)]
pub struct FogInst {
    pub actor: String,
    pub xf: Xf,
    pub visible: bool,
    pub density: f32,
    pub height_falloff: f32,
    pub max_opacity: f32,
    pub start_distance: f32,
    pub cutoff_distance: f32,
    pub inscattering_color: [f32; 4],
    pub inscattering_texture_tint: [f32; 4],
    pub directional_color: [f32; 4],
    pub directional_exponent: f32,
    pub directional_start_distance: f32,
    pub fully_directional_distance: f32,
    pub non_directional_distance: f32,
    pub volumetric: bool,
    pub volumetric_distance: f32,
    pub volumetric_scattering_distribution: f32,
    pub volumetric_scattering_scale: f32,
    pub volumetric_absorption_scale: f32,
    pub override_light_colors: bool,
}

#[derive(Clone, Debug)]
pub struct StaticInst {
    pub actor: String,
    pub component: String,
    pub export: usize,
    /// `/Game/..Pkg.Obj` of the `UStaticMesh`.
    pub mesh: String,
    /// Component-to-world transform in UE space (the streaming-level offset included).
    pub xf: Xf,
    /// `OverrideMaterials` by slot (`None` = the mesh's own material).
    pub material_overrides: Vec<Option<String>>,
    /// `bVisible && !bHiddenInGame && !actor.bHidden`.
    pub visible: bool,
    pub cast_shadow: bool,
    pub affects_distance_field: bool,
    pub max_draw_distance: f32,
    pub translucency_sort_priority: i32,
    /// `level actor` or `blueprint <BP_X_C>`.
    pub source: String,
}

#[derive(Clone, Debug)]
pub struct ParticleInst { pub actor: String, pub component: String, pub template: Option<String>, pub xf: Xf, pub visible: bool, pub auto_activate: bool }

#[derive(Clone, Debug)]
pub struct SkeletalInst { pub actor: String, pub mesh: Option<String>, pub xf: Xf, pub materials: Vec<Option<String>>, pub note: String, pub hidden: bool }

#[derive(Clone, Debug)]
pub struct CameraInfo {
    pub actor: String,
    pub actor_class: String,
    /// The `UCameraComponent` the view uses: the actor's native root-attached `CameraComponent` (the child `CameraActor_0` points down -X and
    /// cannot see the pads; see SCENE.md).
    pub xf: Xf,
    pub fov_deg: f32,
    /// `bConstrainAspectRatio` / `AspectRatio` as serialized or defaulted.
    pub aspect_ratio: f32,
    pub post_process_blend_weight: f32,
    /// The BP's other camera component (`CameraActor_0`, FOV 35 child): recorded, not used.
    pub child_camera: Option<(Xf, f32)>,
}

/// One `PartyCharacterPlacementHelper_C` actor (`Player0..4`): a party slot of the lobby.
#[derive(Clone, Debug)]
pub struct PlacementInfo {
    pub actor: String,
    pub player_id: i32,
    pub hidden: bool,
    pub player_on_pad: bool,
    /// `Root` scene component (the pad).
    pub root: Xf,
    /// `CharacterPlacement` component: where the hero's skeletal mesh stands (feet origin; rotator identity yaw = faces the camera).
    pub character: Xf,
    /// World transforms of the UMG world widgets of the slot (`LobbyPlayerPadTop` name plate, `LobbyPlayerAddPlayer` "+" marker,
    /// `LobbyPlayerPadGadgets`): the UI half anchors its name plate / "+" markers at these points (`EWidgetSpace::Screen`).
    pub top_widget: Xf,
    pub add_widget: Xf,
    pub gadgets_widget: Xf,
    /// `UWidgetComponent` data of the name plate / "+" components (template + instance): Space, Pivot, DrawSize, bDrawAtDesiredSize, WidgetClass.
    pub top_info: WidgetCompInfo,
    pub add_info: WidgetCompInfo,
}

/// The `UWidgetComponent` properties that decide where a screen-space world widget lands (UE 4.16 `SWorldWidgetScreenLayer`: the component
/// location is projected, the widget slot then uses `Alignment = Pivot` and (if `bDrawAtDesiredSize`) its desired size, else `DrawSize`).
#[derive(Clone, Debug, PartialEq)]
pub struct WidgetCompInfo {
    pub widget_class: String,
    /// `EWidgetSpace::Screen` (true) or `World`
    pub screen_space: bool,
    /// serialized `Pivot`, else the `UWidgetComponent` ctor default (0.5, 0.5) (exe RVA 0x1ba41a0: `unpcklps xmm1(0.5)`; `movsd [this+0x7ac]` at 0x1ba441f)
    pub pivot: [f32; 2],
    /// serialized `DrawSize`, else the ctor default 500 x 500 (exe 0x1ba41fb / 0x1ba4205)
    pub draw_size: [f32; 2],
    pub draw_at_desired_size: bool,
}

#[derive(Clone, Debug)]
pub struct PostVolumeInfo {
    pub name: String,
    pub map: String,
    pub priority: f32,
    pub blend_radius: f32,
    pub blend_weight: f32,
    pub enabled: bool,
    pub unbound: bool,
    /// World-space axis aligned bounds `(center, half extents)` from the brush's body setup convex box (`None` = unbound / unrecovered).
    pub bounds: Option<(Vec3, Vec3)>,
    pub settings: PropList,
    /// `ColorGradingLUT` object path of the volume's settings (used when `bOverride_ColorGradingLUT`).
    pub lut: Option<String>,
    /// `WeightedBlendables` of the settings (post-process materials) as `(object path, weight)`.
    pub blendables: Vec<(String, f32)>,
}

/// `FPostProcessSettings` after stacking the volumes that contain the camera (higher `Priority` overrides, per `bOverride_*` flag; a flag
/// with no serialized value means the exe constructor default). Only the fields the lobby renderer consumes are named.
#[derive(Clone, Debug, Default)]
pub struct PostSettings {
    pub f: BTreeMap<String, Vec<f32>>,
    pub color_grading_lut: Option<String>,
    pub blendables: Vec<(String, f32)>,
    pub dof_method: String,
    /// Names of the fields some volume set (`bOverride_X` true).
    pub overridden: Vec<String>,
}
impl PostSettings {
    pub fn get(&self, name: &str) -> &[f32] { self.f.get(name).map(|v| v.as_slice()).unwrap_or(&[]) }
    pub fn f1(&self, name: &str) -> f32 { self.get(name).first().copied().unwrap_or(0.) }
    pub fn v4(&self, name: &str) -> [f32; 4] { let v = self.get(name); [*v.first().unwrap_or(&1.), *v.get(1).unwrap_or(&1.), *v.get(2).unwrap_or(&1.), *v.get(3).unwrap_or(&1.)] }
}

/// What decides which lobby level the front end shows (all from the packages; see SCENE.md).
#[derive(Clone, Debug)]
pub struct LobbySelection {
    /// `(WorldAsset, LevelTransform translation)` of every streaming level of `/Game/Maps/Frontend`.
    pub frontend_streaming: Vec<(String, String, Vec3)>,
    /// `Default__Frontend_Lobby_C.LobbySelect` (`FortniteLobby::NewEnumerator1`).
    pub lobby_select: String,
    /// Display name of that enumerator in `FortniteLobby` (`Halloween`).
    pub lobby_select_label: String,
    /// The chosen level.
    pub map: String,
    /// `FrontEndSettingsBP_31.Cameras`: `(EFrontEndCamera::X, actor)`.
    pub camera_pairs: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct LobbySceneData {
    pub selection: Option<LobbySelection>,
    pub statics: Vec<StaticInst>,
    pub lights: Vec<LightInst>,
    pub fogs: Vec<FogInst>,
    pub particles: Vec<ParticleInst>,
    pub skeletals: Vec<SkeletalInst>,
    pub post_volumes: Vec<PostVolumeInfo>,
    pub camera: Option<CameraInfo>,
    pub placements: Vec<PlacementInfo>,
    /// Components of the lobby that exist in the data and are NOT drawn by this port (class, actor:component, why).
    pub unrendered: Vec<(String, String, String)>,
    /// Things that could not be recovered.
    pub missing: Vec<String>,
    /// Post-process volumes' result at the camera.
    pub post: PostSettings,
    /// COSMETICS view only: `CharacterPlacement` of the `VaultWeaponPlacementHelper_C` actor (where an inspected item (glider / harvesting tool) floats).
    pub item_preview: Option<Xf>,
    /// Non-lobby views: the `MaterialCollection0` uniform buffer of the view's materials (`read_mpc_defaults`); empty = none.
    pub mpc: Vec<[f32; 4]>,
    pub matinee: Option<MatineeInfo>,
}

#[derive(Clone, Debug, Default)]
pub struct CurvePoint { pub t: f32, pub v: Vec3, pub arrive: Vec3, pub leave: Vec3, pub mode: String }

#[derive(Clone, Debug)]
pub struct MatineeAnimKey { pub start: f32, pub seq: String, pub start_offset: f32, pub end_offset: f32, pub rate: f32, pub looping: bool, pub reverse: bool }

#[derive(Clone, Debug, Default)]
pub struct MatineeGroup { pub name: String, pub actor: String, pub pos: Vec<CurvePoint>, pub euler: Vec<CurvePoint>, pub anims: Vec<MatineeAnimKey>, pub vis: Vec<(f32, bool)> }

#[derive(Clone, Debug, Default)]
pub struct MatineeInfo { pub actor: String, pub length: f32, pub looping: bool, pub play_on_load: bool, pub groups: Vec<MatineeGroup> }

pub fn eval_interp_curve(pts: &[CurvePoint], t: f32) -> Option<Vec3> {
    let first = pts.first()?;
    if pts.len() == 1 || t <= first.t { return Some(first.v); }
    let last = pts.last()?;
    if t >= last.t { return Some(last.v); }
    let i = pts.iter().rposition(|p| p.t <= t).unwrap_or(0).min(pts.len() - 2);
    let (a, b) = (&pts[i], &pts[i + 1]);
    let diff = b.t - a.t;
    if diff <= 0.0 { return Some(b.v); }
    let alpha = (t - a.t) / diff;
    Some(match a.mode.as_str() {
        "CIM_Constant" => a.v,
        "CIM_Linear" => a.v.lerp(b.v, alpha),
        _ => {
            let (p0, t0, p1, t1) = (a.v, a.leave * diff, b.v, b.arrive * diff);
            let (a2, a3) = (alpha * alpha, alpha * alpha * alpha);
            p0 * (2.0 * a3 - 3.0 * a2 + 1.0) + t0 * (a3 - 2.0 * a2 + alpha) + t1 * (a3 - a2) + p1 * (-2.0 * a3 + 3.0 * a2)
        }
    })
}

impl MatineeInfo {
    pub fn position(&self, scene_t: f32) -> f32 {
        if self.length <= 0.0 { return 0.0; }
        if self.looping { scene_t.rem_euclid(self.length) } else { scene_t.min(self.length) }
    }
}

impl MatineeGroup {
    pub fn transform(&self, t: f32) -> Option<(Vec3, [f32; 3])> {
        let p = eval_interp_curve(&self.pos, t)?;
        let e = eval_interp_curve(&self.euler, t).unwrap_or(Vec3::ZERO);
        Some((p, [e.y, e.z, e.x]))
    }
    pub fn anim(&self, t: f32, length_of: impl Fn(&str) -> Option<f32>) -> Option<(&str, f32)> {
        let i = self.anims.iter().rposition(|k| k.start <= t);
        let k = match i { Some(i) => &self.anims[i], None => { let k = self.anims.first()?; return Some((&k.seq, k.start_offset)); } };
        let len = length_of(&k.seq)?;
        let end = (len - k.end_offset).max(k.start_offset);
        let span = (end - k.start_offset).max(0.0);
        let mut local = (t - k.start) * k.rate;
        if k.looping && span > 0.0 { local = local.rem_euclid(span); } else { local = local.clamp(0.0, span); }
        let at = if k.reverse { end - local } else { k.start_offset + local };
        Some((&k.seq, at))
    }
    pub fn visible(&self, t: f32, first_loop: bool, initially_hidden: bool) -> bool {
        match self.vis.iter().rev().find(|(kt, _)| *kt <= t) {
            Some((_, show)) => *show,
            None if first_loop || self.vis.is_empty() => !initially_hidden,
            None => self.vis.last().map_or(!initially_hidden, |(_, show)| *show),
        }
    }
}

/// Actor property helper: the actor's own merged class defaults and instance values.
struct ActorInfo { name: String, class: String, class_path: Option<String>, hidden: bool, bp: Option<Rc<BpClass>> }

struct MapReader<'a> {
    set: &'a fn_pak::PakSet,
    view: PkgView,
    offset: Vec3,
    bps: RefCell<HashMap<String, Option<Rc<BpClass>>>>,
    label: String,
}

impl<'a> MapReader<'a> {
    fn open(set: &'a fn_pak::PakSet, long: &str, offset: Vec3) -> Result<MapReader<'a>, String> {
        Ok(MapReader { set, view: PkgView::open(set, long, true)?, offset, bps: RefCell::new(HashMap::new()), label: long.rsplit('/').next().unwrap_or(long).to_owned() })
    }

    fn bp(&self, class_path: &str) -> Option<Rc<BpClass>> {
        if !class_path.starts_with("/Game/") { return None; }
        if let Some(c) = self.bps.borrow().get(class_path) { return c.clone(); }
        let r = self.load_bp(class_path);
        self.bps.borrow_mut().insert(class_path.to_owned(), r.clone());
        r
    }
    fn load_bp(&self, class_path: &str) -> Option<Rc<BpClass>> {
        let (long, class_name) = class_path.split_once('.')?;
        let view = PkgView::open(self.set, long, false).ok()?;
        let class_export = (0..view.n()).find(|&i| view.name(i) == class_name && view.class(i).ends_with("GeneratedClass"))?;
        let cdo = (0..view.n()).find(|&i| view.name(i) == format!("Default__{class_name}"));
        let sup = view.pkg.exports[class_export].super_index;
        let parent = if sup < 0 { let p = view.pkg.import_path(sup); self.bp(&p) } else { None };
        let mut scs = HashMap::new();
        for i in 0..view.n() {
            if view.class(i) != "SCS_Node" { continue; }
            let p = view.props(i);
            if let (Some(name), Some(t)) = (p.text("InternalVariableName"), view.export_of(p.get("ComponentTemplate"))) { scs.insert(name.to_owned(), t); }
        }
        Some(Rc::new(BpClass { view, class_export, cdo, parent, scs }))
    }

    fn actor_info(&self, a: usize) -> ActorInfo {
        let class_path = self.view.class_path(a);
        let props = self.view.props(a);
        ActorInfo {
            name: self.view.name(a).to_owned(),
            class: self.view.class(a),
            bp: class_path.as_deref().and_then(|p| self.bp(p)),
            class_path,
            hidden: props.bool("bHidden").unwrap_or(false),
        }
    }

    /// Instance properties over the Blueprint template (class defaults) of component `comp`.
    fn comp_props(&self, comp: usize) -> PropList {
        let mut out = PropList::default();
        if let Some(a) = self.view.owner(comp) {
            if let Some(bp) = self.view.class_path(a).as_deref().and_then(|p| self.bp(p)) { out = bp.merged(self.view.name(comp)); }
        }
        overlay(&mut out, &self.view.props(comp));
        out
    }

    fn rel_xf(p: &PropList) -> Xf {
        Xf::from_pyr(pv_vec3(p, "RelativeLocation").unwrap_or([0.; 3]), pv_vec3(p, "RelativeRotation").unwrap_or([0.; 3]), pv_vec3(p, "RelativeScale3D").unwrap_or([1.; 3]))
    }

    /// Component-to-world transform (centimetres), composed through `AttachParent`; the streaming-level offset is added at the chain's root.
    fn world_xf(&self, comp: usize) -> Xf {
        let mut chain = vec![];
        let mut cur = Some(comp);
        while let Some(c) = cur {
            if chain.len() > 32 || chain.contains(&c) { break; }
            chain.push(c);
            let p = self.comp_props(c);
            cur = self.view.export_of(p.get("AttachParent"));
        }
        let mut world = Xf::IDENTITY;
        for &c in chain.iter().rev() { world = Self::rel_xf(&self.comp_props(c)).compose(&world); }
        world.t += self.offset;
        world
    }

    fn comp_visible(&self, p: &PropList, actor: &ActorInfo) -> bool {
        p.bool("bVisible").unwrap_or(true) && !p.bool("bHiddenInGame").unwrap_or(false) && !actor.hidden
    }

    fn is_actor(&self, i: usize) -> bool {
        let c = self.view.class(i);
        !c.ends_with("Component") && !c.ends_with("Property") && !matches!(c.as_str(), "Function" | "Model" | "BodySetup" | "InterpGroup" | "InterpData" | "SCS_Node" | "SimpleConstructionScript")
            && self.view.pkg.exports[i].outer_index > 0
            && self.view.class(self.view.pkg.exports[i].outer_index as usize - 1) == "Level"
    }
}
fn paths_of_array(view: &PkgView, v: Option<&PropValue>) -> Vec<Option<String>> {
    match v {
        Some(PropValue::Array(a)) => a.iter().map(|x| match x { PropValue::Object(0) => None, other => view.soft_or_hard_path(Some(other)) }).collect(),
        Some(other) => vec![view.soft_or_hard_path(Some(other))],
        None => vec![],
    }
}

fn blackbody_linear(temp_k: f32) -> [f32; 3] {
    // FLinearColor::MakeFromColorTemperature (UE 4.16 Color.cpp / LinearColor): Planckian locus approximation in CIE 1960 UCS, BT.709 primaries.
    let t = temp_k.clamp(1000., 15000.) as f64;
    let u = (0.860117757 + 1.54118254e-4 * t + 1.28641212e-7 * t * t) / (1.0 + 8.42420235e-4 * t + 7.08145163e-7 * t * t);
    let v = (0.317398726 + 4.22806245e-5 * t + 4.20481691e-8 * t * t) / (1.0 - 2.89741816e-5 * t + 1.61456053e-7 * t * t);
    let (x, y) = (3. * u / (2. * u - 8. * v + 4.), 2. * v / (2. * u - 8. * v + 4.));
    let z = 1. - x - y;
    let (cx, cy, cz) = (1. / y * x, 1.0, 1. / y * z);
    [
        (3.2404542 * cx - 1.5371385 * cy - 0.4985314 * cz) as f32,
        (-0.9692660 * cx + 1.8760108 * cy + 0.0415560 * cz) as f32,
        (0.0556434 * cx - 0.2040259 * cy + 1.0572252 * cz) as f32,
    ]
}

impl LightInst {
    /// Linear light colour: `LightColor` (sRGB FColor to linear) times the black-body colour when `bUseTemperature`.
    pub fn linear_color(&self) -> [f32; 3] {
        let lin = |c: u8| { let c = c as f32 / 255.; if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } };
        let mut c = [lin(self.color_srgb[0]), lin(self.color_srgb[1]), lin(self.color_srgb[2])];
        if self.use_temperature { let b = blackbody_linear(self.temperature); for i in 0..3 { c[i] *= b[i]; } }
        c
    }
}

impl<'a> MapReader<'a> {
    fn actors(&self) -> Vec<usize> { (0..self.view.n()).filter(|&i| self.is_actor(i)).collect() }

    fn light_from(&self, kind: LightKind, actor: &ActorInfo, comp: usize, p: &PropList) -> LightInst {
        let d = ExeDefaults::get();
        let (dir, sky) = ("UDirectionalLightComponent", "USkyLightComponent");
        let intensity = pv_f32(p, "Intensity").unwrap_or(match kind {
            LightKind::Directional => d.class1(dir, "Intensity"),
            LightKind::Sky => d.class1(sky, "Intensity"),
            _ => point_defaults::INTENSITY,
        });
        let color = pv_color(p, "LightColor").unwrap_or([255, 255, 255, 255]);
        LightInst {
            actor: actor.name.clone(),
            component: self.view.name(comp).to_owned(),
            kind,
            xf: self.world_xf(comp),
            visible: self.comp_visible(p, actor),
            affects_world: p.bool("bAffectsWorld").unwrap_or(true),
            intensity,
            color_srgb: [color[0], color[1], color[2]],
            use_temperature: p.bool("bUseTemperature").unwrap_or(false),
            temperature: pv_f32(p, "Temperature").unwrap_or(d.class1(dir, "Temperature")),
            attenuation_radius: pv_f32(p, "AttenuationRadius").unwrap_or(point_defaults::ATTENUATION_RADIUS),
            falloff_exponent: pv_f32(p, "LightFalloffExponent").unwrap_or(point_defaults::FALLOFF_EXPONENT),
            inverse_squared: p.bool("bUseInverseSquaredFalloff").unwrap_or(true),
            source_radius: pv_f32(p, "SourceRadius").unwrap_or(point_defaults::SOURCE_RADIUS),
            source_length: pv_f32(p, "SourceLength").unwrap_or(point_defaults::SOURCE_LENGTH),
            inner_cone_deg: pv_f32(p, "InnerConeAngle").unwrap_or(point_defaults::SPOT_INNER_CONE_DEG),
            outer_cone_deg: pv_f32(p, "OuterConeAngle").unwrap_or(point_defaults::SPOT_OUTER_CONE_DEG),
            cast_shadows: p.bool("CastShadows").unwrap_or(true),
            cascades: p.int("DynamicShadowCascades").map(|v| v as i32).unwrap_or(d.class1(dir, "DynamicShadowCascades") as i32),
            shadow_distance: pv_f32(p, "DynamicShadowDistanceMovableLight").unwrap_or(d.class1(dir, "DynamicShadowDistanceMovableLight")),
            light_source_angle: pv_f32(p, "LightSourceAngle").unwrap_or(d.class1(dir, "LightSourceAngle")),
            volumetric_scattering: pv_f32(p, "VolumetricScatteringIntensity").unwrap_or(1.0),
            indirect_intensity: pv_f32(p, "IndirectLightingIntensity").unwrap_or(1.0),
            max_draw_distance: pv_f32(p, "MaxDrawDistance").unwrap_or(0.),
            max_distance_fade_range: pv_f32(p, "MaxDistanceFadeRange").unwrap_or(0.),
            mobility: p.text("Mobility").map(str::to_owned),
            light_function: self.view.soft_or_hard_path(p.get("LightFunctionMaterial")),
            ies_texture: self.view.soft_or_hard_path(p.get("IESTexture")),
            lighting_channels: p.get("LightingChannels").map(|v| format!("{v:?}")),
            specular_scale: pv_f32(p, "SpecularScale").unwrap_or(1.0),
            cubemap: self.view.soft_or_hard_path(p.get("Cubemap")),
            cubemap_angle_deg: pv_f32(p, "SourceCubemapAngle").unwrap_or(0.),
            lower_hemisphere_color: pv_vec4(p, "LowerHemisphereColor").map(|c| [c[0], c[1], c[2]]).unwrap_or([0., 0., 0.]),
            occlusion_max_distance: pv_f32(p, "OcclusionMaxDistance").unwrap_or(d.class1(sky, "OcclusionMaxDistance")),
            source: match &actor.class_path { Some(c) if c.starts_with("/Game/") => format!("blueprint {}", c.rsplit('.').next().unwrap_or(c)), _ => "level actor".into() },
        }
    }

    fn fog_from(&self, actor: &ActorInfo, comp: usize, p: &PropList) -> FogInst {
        let d = ExeDefaults::get();
        let f = "UExponentialHeightFogComponent";
        let c4 = |name: &str, def: &str| pv_vec4(p, name).unwrap_or_else(|| { let v = d.class(f, def).unwrap_or(&[1., 1., 1., 1.]); [v[0], v[1], v[2], v[3]] });
        FogInst {
            actor: actor.name.clone(),
            xf: self.world_xf(comp),
            visible: self.comp_visible(p, actor),
            density: pv_f32(p, "FogDensity").unwrap_or(d.class1(f, "FogDensity")),
            height_falloff: pv_f32(p, "FogHeightFalloff").unwrap_or(d.class1(f, "FogHeightFalloff")),
            max_opacity: pv_f32(p, "FogMaxOpacity").unwrap_or(d.class1(f, "FogMaxOpacity")),
            start_distance: pv_f32(p, "StartDistance").unwrap_or(d.class1(f, "StartDistance")),
            cutoff_distance: pv_f32(p, "FogCutoffDistance").unwrap_or(d.class1(f, "FogCutoffDistance")),
            inscattering_color: c4("FogInscatteringColor", "FogInscatteringColor"),
            inscattering_texture_tint: c4("InscatteringTextureTint", "InscatteringTextureTint"),
            directional_color: c4("DirectionalInscatteringColor", "DirectionalInscatteringColor"),
            directional_exponent: pv_f32(p, "DirectionalInscatteringExponent").unwrap_or(d.class1(f, "DirectionalInscatteringExponent")),
            directional_start_distance: pv_f32(p, "DirectionalInscatteringStartDistance").unwrap_or(d.class1(f, "DirectionalInscatteringStartDistance")),
            fully_directional_distance: pv_f32(p, "FullyDirectionalInscatteringColorDistance").unwrap_or(d.class1(f, "FullyDirectionalInscatteringColorDistance")),
            non_directional_distance: pv_f32(p, "NonDirectionalInscatteringColorDistance").unwrap_or(d.class1(f, "NonDirectionalInscatteringColorDistance")),
            volumetric: p.bool("bEnableVolumetricFog").unwrap_or(false),
            volumetric_distance: pv_f32(p, "VolumetricFogDistance").unwrap_or(d.class1(f, "VolumetricFogDistance")),
            volumetric_scattering_distribution: pv_f32(p, "VolumetricFogScatteringDistribution").unwrap_or(d.class1(f, "VolumetricFogScatteringDistribution")),
            volumetric_scattering_scale: pv_f32(p, "VolumetricFogScatteringScale").unwrap_or(d.class1(f, "VolumetricFogScatteringScale")),
            volumetric_absorption_scale: pv_f32(p, "VolumetricFogAbsorptionScale").unwrap_or(d.class1(f, "VolumetricFogAbsorptionScale")),
            override_light_colors: p.bool("bOverrideLightColorsWithFogInscatteringColors").unwrap_or(false),
        }
    }

    /// Everything renderable / lighting related of one actor of this level into `out`.
    fn collect_actor(&self, a: usize, out: &mut LobbySceneData) {
        let actor = self.actor_info(a);
        let bp_name = actor.class_path.as_deref().filter(|c| c.starts_with("/Game/")).map(|c| c.rsplit('.').next().unwrap_or(c).to_owned());
        let source = match &bp_name { Some(n) => format!("blueprint {n}"), None => "level actor".into() };
        for c in self.view.children(a) {
            let class = self.view.class(c);
            let p = self.comp_props(c);
            let cname = self.view.name(c).to_owned();
            let tag = format!("{}:{}", actor.name, cname);
            match class.as_str() {
                "StaticMeshComponent" => {
                    let Some(mesh) = self.view.soft_or_hard_path(p.get("StaticMesh")) else { continue };
                    let materials = paths_of_array(&self.view, p.get("OverrideMaterials"));
                    out.statics.push(StaticInst {
                        actor: actor.name.clone(), component: cname, export: c, mesh, xf: self.world_xf(c), material_overrides: materials,
                        visible: self.comp_visible(&p, &actor),
                        cast_shadow: p.bool("CastShadow").unwrap_or(true) && p.bool("bCastDynamicShadow").unwrap_or(true),
                        affects_distance_field: p.bool("bAffectDistanceFieldLighting").unwrap_or(true),
                        max_draw_distance: pv_f32(&p, "LDMaxDrawDistance").or(pv_f32(&p, "CachedMaxDrawDistance")).unwrap_or(0.),
                        translucency_sort_priority: p.int("TranslucencySortPriority").unwrap_or(0) as i32,
                        source: source.clone(),
                    });
                }
                "FoliageInstancedStaticMeshComponent" | "InstancedStaticMeshComponent" | "HierarchicalInstancedStaticMeshComponent" => self.collect_instanced(a, c, &actor, &p, &source, out),
                "PointLightComponent" => out.lights.push(self.light_from(LightKind::Point, &actor, c, &p)),
                "SpotLightComponent" => out.lights.push(self.light_from(LightKind::Spot, &actor, c, &p)),
                "DirectionalLightComponent" => out.lights.push(self.light_from(LightKind::Directional, &actor, c, &p)),
                "SkyLightComponent" => out.lights.push(self.light_from(LightKind::Sky, &actor, c, &p)),
                "ExponentialHeightFogComponent" => out.fogs.push(self.fog_from(&actor, c, &p)),
                "ParticleSystemComponent" => out.particles.push(ParticleInst {
                    actor: actor.name.clone(), component: cname, template: self.view.soft_or_hard_path(p.get("Template")), xf: self.world_xf(c),
                    visible: self.comp_visible(&p, &actor), auto_activate: p.bool("bAutoActivate").unwrap_or(true),
                }),
                "SkeletalMeshComponent" => out.skeletals.push(SkeletalInst {
                    actor: actor.name.clone(), mesh: self.view.soft_or_hard_path(p.get("SkeletalMesh")), xf: self.world_xf(c),
                    materials: paths_of_array(&self.view, p.get("OverrideMaterials")),
                    note: "driven by MatineeActor_Halloween (InterpData_0, 120.25 s loop, bPlayOnLevelLoad)".into(), hidden: actor.hidden,
                }),
                "WidgetComponent" => out.unrendered.push((class, tag, "UMG world widget (pad gadgets / add-player); belongs to the UI half".into())),
                "TextRenderComponent" | "AudioComponent" | "ArrowComponent" | "BillboardComponent" | "BrushComponent" | "BoxComponent" | "TimelineComponent" => {}
                "CameraComponent" | "SceneComponent" | "PostProcessComponent" | "InterpTrackMove" => {}
                other if other.ends_with("Component") => out.unrendered.push((class.clone(), tag, format!("{other} is not handled by this port"))),
                _ => {}
            }
        }
    }

    /// `FoliageInstancedStaticMeshComponent`: the per-instance transforms are native data after the tagged properties
    /// (`[i32 LODData count][i32 elementSize, i32 count, count * FInstancedStaticMeshInstanceData{FMatrix, 2 x FVector2D}][cluster tree]`).
    fn collect_instanced(&self, a: usize, c: usize, actor: &ActorInfo, p: &PropList, source: &str, out: &mut LobbySceneData) {
        let tag = format!("{}:{}", actor.name, self.view.name(c));
        let Some(mesh) = self.view.soft_or_hard_path(p.get("StaticMesh")) else {
            out.unrendered.push(("InstancedStaticMeshComponent".into(), tag, "no StaticMesh".into())); return;
        };
        let n = p.int("NumBuiltInstances").unwrap_or(0) as usize;
        // find the end of the tagged properties: re-read them and keep the reader position
        let (start, end) = self.view.pkg.export_range(c);
        let ctx = props::Ctx { names: &self.view.pkg.names, file_version: self.view.pkg.summary.file_version };
        let mut r = Reader::at(&self.view.pkg.data[..end], start);
        if props::read_props(&mut r, &ctx).is_err() { out.missing.push(format!("{tag}: properties unreadable, {n} instances not placed")); return; }
        let parse = |r: &mut Reader| -> Result<Vec<Mat4>, String> {
            let head: Vec<u8> = r.data[r.pos..(r.pos + 48).min(r.data.len())].to_vec();
            let lod = r.i32().map_err(|e| e.to_string())?;
            if lod != 0 { return Err(format!("{lod} LOD data blocks before the instance array")); }
            let spare = r.i32().map_err(|e| e.to_string())?; // a second zero i32 precedes the bulk array in the 1.8 cook (verified on the lobby foliage)
            if spare != 0 { return Err(format!("unexpected spare value {spare} before the instance array")); }
            let elem = r.i32().map_err(|e| e.to_string())? as usize;
            let count = r.i32().map_err(|e| e.to_string())? as usize;
            if elem != 80 { return Err(format!("instance element size {elem}, expected 80 (bytes after the properties: {head:02x?})")); }
            let mut v = Vec::with_capacity(count);
            for _ in 0..count {
                let mut f = [0f32; 20];
                for x in f.iter_mut() { *x = r.f32().map_err(|e| e.to_string())?; }
                // FMatrix is row-major with the translation in row 3: glam columns = the UE rows
                v.push(Mat4::from_cols_array(&[f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7], f[8], f[9], f[10], f[11], f[12], f[13], f[14], f[15]]));
            }
            Ok(v)
        };
        let mats = match parse(&mut r) { Ok(m) => m, Err(e) => { out.missing.push(format!("{tag}: {e}; {n} instances not placed")); return; } };
        if mats.len() != n && n != 0 { out.missing.push(format!("{tag}: NumBuiltInstances {n} but {} matrices", mats.len())); }
        let parent = self.world_xf(c);
        let materials = paths_of_array(&self.view, p.get("OverrideMaterials"));
        let visible = self.comp_visible(p, actor);
        for (k, m) in mats.iter().enumerate() {
            let (s, q, t) = m.to_scale_rotation_translation();
            out.statics.push(StaticInst {
                actor: actor.name.clone(), component: format!("{}[{k}]", self.view.name(c)), export: c, mesh: mesh.clone(),
                xf: Xf { s, q, t }.compose(&parent), material_overrides: materials.clone(), visible,
                cast_shadow: p.bool("CastShadow").unwrap_or(true) && p.bool("bCastDynamicShadow").unwrap_or(true), affects_distance_field: p.bool("bAffectDistanceFieldLighting").unwrap_or(true),
                max_draw_distance: pv_f32(p, "LDMaxDrawDistance").unwrap_or(0.), translucency_sort_priority: 0,
                source: format!("{source} (instanced foliage)"),
            });
        }
        let _ = a;
    }

    /// `BP_FortniteLobbyLightSwitcher_C` (`EnableLobbySetLighting` / `SetVisiblityofSceneLighting(true)` run when the HeroSelect camera
    /// activates, see the disassembly in SCENE.md): the full-detail directional light, the sky light, the listed point lights, the fog and the
    /// listed particle systems become visible; the low-detail directional light is the shadow-quality-0 alternative and stays hidden.
    fn apply_light_switcher(&self, out: &mut LobbySceneData) {
        for a in self.actors() {
            if self.view.class(a) != "BP_FortniteLobbyLightSwitcher_C" { continue; }
            let p = self.view.props(a);
            let name_of = |v: Option<&PropValue>| self.view.export_of(v).map(|i| self.view.name(i).to_owned());
            let mut on: Vec<String> = vec![];
            for key in ["DirectionLight", "SkyLight", "ExponentialHeightfog"] { if let Some(n) = name_of(p.get(key)) { on.push(n); } }
            if let Some(PropValue::Array(arr)) = p.get("Pointlights") { for v in arr { if let Some(n) = name_of(Some(v)) { on.push(n); } } }
            let mut particles: Vec<String> = vec![];
            if let Some(PropValue::Array(arr)) = p.get("ParticleSystems") { for v in arr { if let Some(n) = name_of(Some(v)) { particles.push(n); } } }
            let low = name_of(p.get("DirectionalLight_LowDetailMode"));
            let full = name_of(p.get("DirectionLight"));
            for l in out.lights.iter_mut() {
                if on.contains(&l.actor) { l.visible = true; }
                if low.as_deref() == Some(l.actor.as_str()) { l.visible = shadow_quality() == 0; }
                if shadow_quality() == 0 && full.as_deref() == Some(l.actor.as_str()) { l.visible = false; }
            }
            for f in out.fogs.iter_mut() { if on.contains(&f.actor) { f.visible = true; } }
            for pt in out.particles.iter_mut() { if particles.contains(&pt.actor) { pt.visible = true; } }
        }
    }

    fn read_matinee(&self, out: &mut LobbySceneData) {
        let Some(m) = (0..self.view.n()).find(|&i| self.view.class(i) == "MatineeActor") else { return };
        let p = self.view.props(m);
        let Some(data) = self.view.export_of(p.get("MatineeData")) else { out.missing.push(format!("{}: no MatineeData", self.view.name(m))); return };
        let dp = self.view.props(data);
        let mut info = MatineeInfo { actor: self.view.name(m).to_owned(), length: pv_f32(&dp, "InterpLength").unwrap_or(0.0), looping: p.bool("bLooping").unwrap_or(false), play_on_load: p.bool("bPlayOnLevelLoad").unwrap_or(false), groups: vec![] };
        let actors: Vec<(String, String)> = match p.get("GroupActorInfos") {
            Some(PropValue::Array(v)) => v.iter().filter_map(|e| match e {
                PropValue::Struct { fields, .. } => {
                    let g = fields.text("ObjectName")?.to_owned();
                    let a = match fields.get("Actors") { Some(PropValue::Array(a)) => a.first().and_then(|o| self.view.export_of(Some(o))).map(|i| self.view.name(i).to_owned()), _ => None }?;
                    Some((g, a))
                }
                _ => None,
            }).collect(),
            _ => vec![],
        };
        let curve = |v: Option<&PropValue>| -> Vec<CurvePoint> {
            let Some(PropValue::Struct { fields, .. }) = v else { return vec![] };
            let Some(PropValue::Array(pts)) = fields.get("Points") else { return vec![] };
            pts.iter().filter_map(|pt| match pt {
                PropValue::Struct { fields: f, .. } => {
                    let v3 = |k: &str| match f.get(k) { Some(PropValue::Vec3(v)) => Vec3::from(*v), _ => Vec3::ZERO };
                    Some(CurvePoint { t: pv_f32(f, "InVal").unwrap_or(0.0), v: v3("OutVal"), arrive: v3("ArriveTangent"), leave: v3("LeaveTangent"), mode: f.text("InterpMode").unwrap_or("CIM_Linear").to_owned() })
                }
                _ => None,
            }).collect()
        };
        for gi in object_indices(dp.array("InterpGroups")) {
            let gp = self.view.props(gi);
            let gname = self.view.name(gi).to_owned();
            let Some(actor) = actors.iter().find(|(g, _)| *g == gname).map(|(_, a)| a.clone()) else { out.missing.push(format!("{}: {gname} has no actor", info.actor)); continue };
            let mut g = MatineeGroup { name: gp.text("GroupName").unwrap_or("").to_owned(), actor, ..Default::default() };
            for ti in object_indices(gp.array("InterpTracks")) {
                let tp = self.view.props(ti);
                match self.view.class(ti).as_str() {
                    "InterpTrackMove" => {
                        for k in ["MoveFrame", "RotMode"] { if let Some(v) = tp.text(k) { out.missing.push(format!("{}: {} {k} = {v} (only IMF_World / IMR_Keyframed are evaluated)", info.actor, g.name)); } }
                        g.pos = curve(tp.get("PosTrack"));
                        g.euler = curve(tp.get("EulerTrack"));
                    }
                    "InterpTrackAnimControl" => {
                        if let Some(PropValue::Array(keys)) = tp.get("AnimSeqs") {
                            for k in keys {
                                let PropValue::Struct { fields: f, .. } = k else { continue };
                                let Some(seq) = self.view.path_of(f.get("AnimSeq")) else { continue };
                                g.anims.push(MatineeAnimKey { start: pv_f32(f, "StartTime").unwrap_or(0.0), seq, start_offset: pv_f32(f, "AnimStartOffset").unwrap_or(0.0), end_offset: pv_f32(f, "AnimEndOffset").unwrap_or(0.0), rate: pv_f32(f, "AnimPlayRate").unwrap_or(1.0), looping: f.bool("bLooping").unwrap_or(false), reverse: f.bool("bReverse").unwrap_or(false) });
                            }
                        }
                    }
                    "InterpTrackVisibility" => {
                        if let Some(PropValue::Array(keys)) = tp.get("VisibilityTrack") {
                            for k in keys {
                                let PropValue::Struct { fields: f, .. } = k else { continue };
                                let t = pv_f32(f, "Time").unwrap_or(0.0);
                                match f.text("Action").unwrap_or("EVTA_Hide") {
                                    "EVTA_Show" => g.vis.push((t, true)),
                                    "EVTA_Hide" => g.vis.push((t, false)),
                                    other => out.missing.push(format!("{}: {} visibility action {other} not evaluated", info.actor, g.name)),
                                }
                            }
                        }
                    }
                    other => out.missing.push(format!("{}: {} track {other} not evaluated", info.actor, g.name)),
                }
            }
            info.groups.push(g);
        }
        out.matinee = Some(info);
    }

    fn collect_post_volumes(&self, out: &mut LobbySceneData) {
        for a in self.actors() {
            if self.view.class(a) != "PostProcessVolume" { continue; }
            let p = self.view.props(a);
            let name = self.view.name(a).to_owned();
            let brush = self.view.children(a).into_iter().find(|&c| self.view.class(c) == "BrushComponent");
            let mut bounds = None;
            if let Some(b) = brush {
                let bp = self.comp_props(b);
                let rel = Self::rel_xf(&bp);
                let body = self.view.export_of(bp.get("BrushBodySetup"));
                if let Some(bs) = body {
                    let bsp = self.view.props(bs);
                    let mut lo = Vec3::splat(f32::MAX);
                    let mut hi = Vec3::splat(f32::MIN);
                    if let Some(agg) = bsp.fields("AggGeom") {
                        for e in agg.array("ConvexElems").into_iter().flatten() {
                            // `FBox` is native-serialized: Min (3 floats), Max (3 floats), IsValid (byte)
                            let bx = e.as_fields().and_then(|f| f.get("ElemBox"));
                            let (mn, mx) = match bx {
                                Some(PropValue::Raw(b)) if b.len() >= 24 => {
                                    let f = |i: usize| f32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]);
                                    (Some([f(0), f(1), f(2)]), Some([f(3), f(4), f(5)]))
                                }
                                Some(PropValue::Struct { fields, .. }) => (pv_vec3(fields, "Min"), pv_vec3(fields, "Max")),
                                _ => (None, None),
                            };
                            if let (Some(mn), Some(mx)) = (mn, mx) { lo = lo.min(Vec3::from(mn)); hi = hi.max(Vec3::from(mx)); }
                        }
                    }
                    if lo.x <= hi.x {
                        let w = rel.compose(&Xf { t: self.offset, ..Xf::IDENTITY });
                        let corners: Vec<Vec3> = (0..8).map(|k| w.transform_point(Vec3::new(if k & 1 == 0 { lo.x } else { hi.x }, if k & 2 == 0 { lo.y } else { hi.y }, if k & 4 == 0 { lo.z } else { hi.z }))).collect();
                        let (mn, mx) = corners.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), c| (a.min(*c), b.max(*c)));
                        bounds = Some(((mn + mx) * 0.5, (mx - mn) * 0.5));
                    }
                }
            }
            out.post_volumes.push(PostVolumeInfo {
                name, map: self.label.clone(), priority: pv_f32(&p, "Priority").unwrap_or(0.), blend_radius: pv_f32(&p, "BlendRadius").unwrap_or(ExeDefaults::get().class1("APostProcessVolume", "BlendRadius")),
                blend_weight: pv_f32(&p, "BlendWeight").unwrap_or(ExeDefaults::get().class1("APostProcessVolume", "BlendWeight")),
                enabled: p.bool("bEnabled").unwrap_or(true), unbound: p.bool("bUnbound").unwrap_or(false), bounds,
                lut: p.fields("Settings").and_then(|s| self.view.path_of(s.get("ColorGradingLUT"))),
                blendables: p.fields("Settings").and_then(|s| s.fields("WeightedBlendables")).map(|wb| wb.array("Array").into_iter().flatten().filter_map(|e| {
                    let f = e.as_fields()?;
                    Some((self.view.path_of(f.get("Object"))?, f.float("Weight").unwrap_or(1.) as f32))
                }).collect()).unwrap_or_default(),
                settings: p.fields("Settings").cloned().unwrap_or_default(),
            });
        }
    }
}

/// Stacks the post-process volumes that contain `at` (ascending priority; a higher priority overrides per `bOverride_*` flag) over the exe
/// `FPostProcessSettings` constructor defaults.
pub fn stack_post_settings(volumes: &[PostVolumeInfo], at: Vec3) -> PostSettings {
    let d = ExeDefaults::get();
    let mut ps = PostSettings::default();
    for (k, v) in d.pps.iter() { ps.f.insert(k.clone(), v.clone()); }
    let mut vols: Vec<&PostVolumeInfo> = volumes.iter().filter(|v| v.enabled && (v.unbound || v.bounds.is_some_and(|(c, h)| (at - c).abs().cmple(h).all()))).collect();
    vols.sort_by(|a, b| a.priority.partial_cmp(&b.priority).unwrap_or(std::cmp::Ordering::Equal));
    for v in vols {
        for prop in &v.settings.0 {
            let Some(field) = prop.name.strip_prefix("bOverride_") else { continue };
            if !matches!(prop.value, PropValue::Bool(true)) { continue; }
            let val: Option<Vec<f32>> = match v.settings.get(field) {
                Some(PropValue::Float(f)) => Some(vec![*f as f32]),
                Some(PropValue::Int(i)) => Some(vec![*i as f32]),
                Some(PropValue::Bool(b)) => Some(vec![*b as u8 as f32]),
                Some(PropValue::Vec4(c)) => Some(c.to_vec()),
                Some(PropValue::Vec3(c)) => Some(c.to_vec()),
                Some(PropValue::Enum(e)) if field == "DepthOfFieldMethod" => { ps.dof_method = e.clone(); None }
                _ => None,
            };
            match field {
                "ColorGradingLUT" => ps.color_grading_lut = v.lut.clone(),
                _ => {
                    let value = val.or_else(|| d.pps(field).map(|s| s.to_vec())).unwrap_or_else(|| vec![0.]);
                    ps.f.insert(field.to_owned(), value);
                }
            }
            if !ps.overridden.iter().any(|n| n == field) { ps.overridden.push(field.to_owned()); }
        }
        // blendables accumulate (they have no override flag)
        ps.blendables.extend(v.blendables.iter().cloned());
    }
    ps
}
/// `UserDefinedEnum.DisplayNameMap`: `(enumerator, display text)` in declaration order.
pub fn read_enum_labels(set: &fn_pak::PakSet, package: &str) -> Result<Vec<(String, String)>, String> {
    let view = PkgView::open(set, package, false)?;
    let i = (0..view.n()).find(|&i| view.class(i) == "UserDefinedEnum").ok_or_else(|| format!("{package}: no UserDefinedEnum export"))?;
    let p = view.props(i);
    let Some(PropValue::Raw(bytes)) = p.get("DisplayNameMap") else { return Err(format!("{package}: DisplayNameMap is not a raw map")) };
    let mut r = Reader::new(bytes);
    let _remove = r.i32().map_err(|e| e.to_string())?;
    let count = r.i32().map_err(|e| e.to_string())?;
    let mut out = vec![];
    for _ in 0..count {
        let idx = r.i32().map_err(|e| e.to_string())?;
        let num = r.i32().map_err(|e| e.to_string())?;
        let key = view.pkg.names.get(idx as usize).cloned().unwrap_or_default();
        let key = if num > 0 { format!("{key}_{}", num - 1) } else { key };
        let _flags = r.u32().map_err(|e| e.to_string())?;
        let history = r.i8().map_err(|e| e.to_string())?;
        if history != 0 { return Err(format!("{package}: FText history type {history} not supported")); }
        let _ns = r.fstring().map_err(|e| e.to_string())?;
        let _k = r.fstring().map_err(|e| e.to_string())?;
        let text = r.fstring().map_err(|e| e.to_string())?;
        out.push((key, text));
    }
    Ok(out)
}

/// Streaming levels of the front-end level: `(object name, WorldAsset package, LevelTransform translation)`.
fn read_frontend_streaming(fe: &MapReader) -> Vec<(String, String, Vec3)> {
    let mut v = vec![];
    for i in 0..fe.view.n() {
        if !fe.view.class(i).starts_with("LevelStreaming") { continue; }
        let p = fe.view.props(i);
        let Some(asset) = fe.view.soft_or_hard_path(p.get("WorldAsset")) else { continue };
        let long = asset.split('.').next().unwrap_or(&asset).to_owned();
        let t = p.fields("LevelTransform").and_then(|f| pv_vec3(f, "Translation")).unwrap_or([0.; 3]);
        v.push((fe.view.name(i).to_owned(), long, Vec3::from(t)));
    }
    v
}

/// `FrontEndSettingsBP_31.Cameras`: `(EFrontEndCamera::X, actor export)`.
fn read_camera_pairs(fe: &MapReader) -> Vec<(String, usize)> {
    let mut out = vec![];
    for i in 0..fe.view.n() {
        if fe.view.class(i) != "FrontEndSettingsBP_C" || fe.view.name(i).starts_with("Default__") { continue; }
        let p = fe.view.props(i);
        for e in p.array("Cameras").into_iter().flatten() {
            let Some(f) = e.as_fields() else { continue };
            if let (Some(ty), Some(actor)) = (f.text("Type"), fe.view.export_of(f.get("Camera"))) { out.push((ty.to_owned(), actor)); }
        }
    }
    out
}

/// Default of `Frontend_Lobby_C.LobbySelect` in the selector level.
fn read_lobby_select(set: &fn_pak::PakSet) -> Result<String, String> {
    let v = PkgView::open(set, LOBBY_SELECTOR_MAP, true)?;
    let cdo = (0..v.n()).find(|&i| v.name(i) == "Default__Frontend_Lobby_C").ok_or("no Default__Frontend_Lobby_C")?;
    let inst = (0..v.n()).find(|&i| v.class(i) == "Frontend_Lobby_C" && !v.name(i).starts_with("Default__"));
    let from_inst = inst.and_then(|i| v.props(i).text("LobbySelect").map(str::to_owned));
    from_inst.or_else(|| v.props(cdo).text("LobbySelect").map(str::to_owned)).ok_or_else(|| "LobbySelect not serialized".to_owned())
}

/// Radius (cm) around the camera inside which other actors of the persistent front-end level would be part of the view.
const NEARBY_RADIUS_CM: f32 = 40_000.0;

/// Reads the whole BR lobby scene from an opened pak set. See `LobbySceneData`.
pub fn read_lobby_scene_from(set: &fn_pak::PakSet) -> Result<LobbySceneData, String> {
    let mut out = LobbySceneData::default();
    let fe = MapReader::open(set, FRONTEND_MAP, Vec3::ZERO)?;
    let streaming = read_frontend_streaming(&fe);
    let lobby_select = read_lobby_select(set)?;
    let labels = read_enum_labels(set, LOBBY_ENUM_PACKAGE)?;
    let enumerator = lobby_select.rsplit("::").next().unwrap_or(&lobby_select).to_owned();
    let label = labels.iter().find(|(k, _)| *k == enumerator).map(|(_, t)| t.clone()).ok_or_else(|| format!("enumerator {enumerator} not in FortniteLobby"))?;
    // Frontend_Lobby_C.ReceiveBeginPlay: case NewEnumerator0 -> LoadStreamLevel(Frontend_Lobby_Default), case NewEnumerator1 -> LoadStreamLevel(Frontend_Lobby_Halloween)
    let map = match label.as_str() { "Halloween" => HALLOWEEN_MAP, "Default" => DEFAULT_LOBBY_MAP, other => return Err(format!("unknown lobby label {other}")) };
    let pairs = read_camera_pairs(&fe);
    out.selection = Some(LobbySelection {
        frontend_streaming: streaming.clone(), lobby_select: lobby_select.clone(), lobby_select_label: label, map: map.to_owned(),
        camera_pairs: pairs.iter().map(|(t, a)| (t.clone(), fe.view.name(*a).to_owned())).collect(),
    });
    let offset_of = |long: &str| streaming.iter().find(|s| s.1 == long).map(|s| s.2).unwrap_or(Vec3::ZERO);

    // ---- the chosen lobby level (every actor) ----
    let lobby = MapReader::open(set, map, offset_of(map))?;
    for a in lobby.actors() { lobby.collect_actor(a, &mut out); }
    lobby.read_matinee(&mut out);
    lobby.apply_light_switcher(&mut out);
    lobby.collect_post_volumes(&mut out);
    // ---- the selector level (its post-process volume; its level blueprint has no renderable actor) ----
    let sel = MapReader::open(set, LOBBY_SELECTOR_MAP, offset_of(LOBBY_SELECTOR_MAP))?;
    sel.collect_post_volumes(&mut out);

    // ---- the persistent front-end level: camera, party slots, anything else within the view radius ----
    let hero_actor = pairs.iter().find(|(t, _)| t.ends_with("::HeroSelect")).map(|(_, a)| *a).ok_or("FrontEndSettings maps no HeroSelect camera")?;
    let cam_actor = fe.actor_info(hero_actor);
    let cam_comp = fe.view.children(hero_actor).into_iter().find(|&c| fe.view.name(c) == "CameraComponent" && fe.view.class(c) == "CameraComponent").ok_or("camera actor has no CameraComponent")?;
    let cp = fe.comp_props(cam_comp);
    let child = fe.view.children(hero_actor).into_iter().find(|&c| fe.view.name(c) == "CameraActor_0" && fe.view.class(c) == "CameraComponent");
    out.camera = Some(CameraInfo {
        actor: cam_actor.name.clone(), actor_class: cam_actor.class.clone(), xf: fe.world_xf(cam_comp),
        fov_deg: pv_f32(&cp, "FieldOfView").unwrap_or(90.), aspect_ratio: pv_f32(&cp, "AspectRatio").unwrap_or(1.777778),
        post_process_blend_weight: pv_f32(&cp, "PostProcessBlendWeight").unwrap_or(1.0),
        child_camera: child.map(|c| { let p = fe.comp_props(c); (fe.world_xf(c), pv_f32(&p, "FieldOfView").unwrap_or(90.)) }),
    });
    let cam_pos = out.camera.as_ref().unwrap().xf.t;
    // the camera actor's own lights / fog (templates say bVisible false: recorded, inactive)
    fe.collect_actor(hero_actor, &mut out);
    for a in fe.actors() {
        if a == hero_actor { continue; }
        let class = fe.view.class(a);
        if class == "PartyCharacterPlacementHelper_C" {
            fe.collect_actor(a, &mut out);
            let ai = fe.actor_info(a);
            let props = fe.view.props(a);
            let find = |name: &str| fe.view.children(a).into_iter().find(|&c| fe.view.name(c) == name);
            let widget = |n: &str| find(n).map(|c| fe.world_xf(c)).unwrap_or(Xf::IDENTITY);
            let winfo = |n: &str| {
                let p = find(n).map(|c| fe.comp_props(c)).unwrap_or_default();
                let v2 = |k: &str, d: [f32; 2]| match p.get(k) { Some(PropValue::Vec2(v)) => *v, _ => d };
                WidgetCompInfo {
                    widget_class: p.text("WidgetClass").unwrap_or("").to_owned(),
                    screen_space: p.text("Space").map_or(false, |s| s.ends_with("Screen")),
                    pivot: v2("Pivot", [0.5, 0.5]), draw_size: v2("DrawSize", [500., 500.]),
                    draw_at_desired_size: p.bool("bDrawAtDesiredSize").unwrap_or(false),
                }
            };
            let (Some(root), Some(place)) = (find("Root"), find("CharacterPlacement")) else { out.missing.push(format!("{}: no Root / CharacterPlacement component", ai.name)); continue };
            out.placements.push(PlacementInfo {
                actor: ai.name.clone(), player_id: props.int("PlayerID").unwrap_or(0) as i32, hidden: ai.hidden,
                player_on_pad: props.bool("PlayerOnPad").unwrap_or(false), root: fe.world_xf(root), character: fe.world_xf(place),
                top_widget: widget("LobbyPlayerPadTop"), add_widget: widget("LobbyPlayerAddPlayer"), gadgets_widget: widget("LobbyPlayerPadGadgets"),
                top_info: winfo("LobbyPlayerPadTop"), add_info: winfo("LobbyPlayerAddPlayer"),
            });
            continue;
        }
        // anything else of the persistent level: only when it is near the camera
        let root = fe.view.export_of(fe.view.props(a).get("RootComponent"));
        let Some(root) = root else { continue };
        let pos = fe.world_xf(root).t;
        if pos.distance(cam_pos) <= NEARBY_RADIUS_CM && !matches!(class.as_str(), "FortWorldSettings" | "LevelBounds" | "Level") {
            out.unrendered.push((class.clone(), fe.view.name(a).to_owned(), format!("persistent-level actor {:.0} cm from the camera: collected generically", pos.distance(cam_pos))));
            fe.collect_actor(a, &mut out);
        }
    }
    out.placements.sort_by_key(|p| p.player_id);
    add_party_grid(&mut out);
    // PartyCharacterPlacementHelper (OnTeamMemberAdded / RefreshWidgets, the `PlayerOnPad` branch of its graph): a pad with a player sets TopSpotLight,
    // RimSpotLight, UnderlightBluePoint01/02 and UnderlightBlue01/02 visible (and the pad material's Occupied = 1); an empty pad keeps them hidden.
    for p in out.placements.iter().filter(|p| p.player_on_pad) {
        for l in out.lights.iter_mut().filter(|l| l.actor == p.actor && matches!(l.component.as_str(), "TopSpotLight" | "RimSpotLight" | "UnderlightBluePoint01" | "UnderlightBluePoint02" | "UnderlightBlue01" | "UnderlightBlue02")) { l.visible = true; }
    }
    // the Frontend persistent level's post volumes (they sit at other y offsets; kept for the containment test)
    fe.collect_post_volumes(&mut out);

    // ---- post-process at the camera ----
    out.post = stack_post_settings(&out.post_volumes, cam_pos);
    if !out.skeletals.is_empty() && out.matinee.is_none() {
        out.missing.push(format!("{} skeletal actors without a MatineeActor: not drawn", out.skeletals.len()));
    }
    Ok(out)
}

pub const GRID_FIRST_ID: i32 = 100;
pub const GRID_SLOTS: usize = 12;
pub const GRID_COLUMNS: usize = 4;
pub const GRID_ROW_Y_CM: [f32; 3] = [125_060.0, 124_915.0, 124_770.0];
pub const GRID_ROW_RISE_CM: f32 = 34.0;
pub const GRID_COL_X_CM: [[f32; 4]; 2] = [[-186.0, -66.0, 64.0, 192.0], [-126.0, -6.0, 126.0, 252.0]];
pub const GRID_CLEAR_CM: f32 = 95.0;
pub fn is_grid_slot(player_id: i32) -> bool { player_id >= GRID_FIRST_ID }
pub fn grid_row(player_id: i32) -> usize { ((player_id - GRID_FIRST_ID).max(0) as usize) / GRID_COLUMNS }

fn add_party_grid(out: &mut LobbySceneData) {
    let mut front: Vec<PlacementInfo> = out.placements.iter().filter(|p| !p.hidden && (0..4).contains(&p.player_id)).cloned().collect();
    if front.len() != 4 { return; }
    front.sort_by(|a, b| a.root.t.x.total_cmp(&b.root.t.x));
    for k in 0..GRID_SLOTS {
        let (row, col) = (k / GRID_COLUMNS, k % GRID_COLUMNS);
        let tpl = &front[col];
        let target = Vec3::new(GRID_COL_X_CM[row % 2][col], GRID_ROW_Y_CM[row], tpl.root.t.z + GRID_ROW_RISE_CM * (row + 1) as f32);
        let d = target - tpl.root.t;
        let id = GRID_FIRST_ID + k as i32;
        let actor = format!("{}#grid{id}", tpl.actor);
        let mv = |x: &Xf| Xf { t: x.t + d, q: x.q, s: x.s };
        let statics: Vec<StaticInst> = out.statics.iter().filter(|s| s.actor == tpl.actor).map(|s| { let mut c = s.clone(); c.actor = actor.clone(); c.xf = mv(&s.xf); c }).collect();
        let lights: Vec<LightInst> = out.lights.iter().filter(|l| l.actor == tpl.actor).map(|l| { let mut c = l.clone(); c.actor = actor.clone(); c.xf = mv(&l.xf); c.visible = false; c }).collect();
        out.statics.extend(statics);
        out.lights.extend(lights);
        out.placements.push(PlacementInfo { actor, player_id: id, hidden: false, player_on_pad: false, root: mv(&tpl.root), character: mv(&tpl.character),
            top_widget: mv(&tpl.top_widget), add_widget: mv(&tpl.add_widget), gadgets_widget: mv(&tpl.gadgets_widget), top_info: tpl.top_info.clone(), add_info: tpl.add_info.clone() });
    }
}

pub fn read_lobby_scene(install: &Path) -> Result<LobbySceneData, String> {
    let set = fn_pak::PakSet::open_install(install).map_err(|e| e.to_string())?;
    read_lobby_scene_from(&set)
}
const BG_MAIN_MAP: &str = "/Game/Maps/UI/Frontend_BG_Main";
/// Radius (cm) around the camera of the `2SegGeoSphere1Unit` backdrop spheres (scale 3000): the backdrop actors of the second copy in `Frontend_BG_Main` (local offset
/// y -10240, i.e. world y 30720 with the level's +40960) are the ones around the Cosmetics camera; the first copy is 10240 cm away behind the sphere's wall.
const COSMETICS_ENV_RADIUS_CM: f32 = 5_000.0;
pub const VAULT_PAWN_MESH_YAW: f32 = -90.0;

/// The scene of the LOCKER (`EFrontEndCamera::Cosmetics`, byte 16 set by `AthenaCustomizationTab::OnActivated`): the camera actor `FrontendCamera_Cosmetics`
/// (`FrontEndSettingsBP_31.Cameras`), the front-end backdrop around it (the duplicate of `Frontend_BG_Main`'s `GenericBackground` sphere / `S_UI_Backplate` / `S_UI_Sphere` actors placed at the
/// Armory (inspect item) region of the persistent level), the Vault's hero placement (`VaultCharacterPlacementHelper_C`, `CharacterPlacement`) as party slot 0, the item preview
/// point (`VaultWeaponPlacementHelper_C`), the Vault lighting blueprint (`VaultCharacterLightingBP_C`) and the post-process volumes at the camera. The lobby's own lights are NOT part of it:
/// the lobby light switcher is turned on by the lobby camera's `OnActivated`.
pub fn read_cosmetics_scene_from(set: &fn_pak::PakSet) -> Result<LobbySceneData, String> {
    let mut out = LobbySceneData::default();
    let fe = MapReader::open(set, FRONTEND_MAP, Vec3::ZERO)?;
    let streaming = read_frontend_streaming(&fe);
    let offset_of = |long: &str| streaming.iter().find(|s| s.1 == long).map(|s| s.2).unwrap_or(Vec3::ZERO);
    let pairs = read_camera_pairs(&fe);
    let cam_actor = pairs.iter().find(|(t, _)| t.ends_with("::Cosmetics")).map(|(_, a)| *a).ok_or("FrontEndSettings maps no Cosmetics camera")?;
    let ai = fe.actor_info(cam_actor);
    let cam_comp = fe.view.children(cam_actor).into_iter().find(|&c| fe.view.name(c) == "CameraComponent" && fe.view.class(c) == "CameraComponent").ok_or("Cosmetics camera actor has no CameraComponent")?;
    let cp = fe.comp_props(cam_comp);
    out.camera = Some(CameraInfo {
        actor: ai.name.clone(), actor_class: ai.class.clone(), xf: fe.world_xf(cam_comp), fov_deg: pv_f32(&cp, "FieldOfView").unwrap_or(90.), aspect_ratio: pv_f32(&cp, "AspectRatio").unwrap_or(1.777778),
        post_process_blend_weight: pv_f32(&cp, "PostProcessBlendWeight").unwrap_or(1.0), child_camera: None,
    });
    let cam_pos = out.camera.as_ref().unwrap().xf.t;
    // the backdrop: every static of Frontend_BG_Main within the sphere around the camera
    let bg = MapReader::open(set, BG_MAIN_MAP, offset_of(BG_MAIN_MAP))?;
    let mut scratch = LobbySceneData::default();
    for a in bg.actors() { bg.collect_actor(a, &mut scratch); }
    out.statics = scratch.statics.into_iter().filter(|s| s.xf.t.distance(cam_pos) <= COSMETICS_ENV_RADIUS_CM).collect();
    out.lights = scratch.lights.into_iter().filter(|l| l.xf.t.distance(cam_pos) <= COSMETICS_ENV_RADIUS_CM).collect();
    // persistent level: the Vault hero placement, the item preview point and the Vault lighting
    for a in fe.actors() {
        let class = fe.view.class(a);
        let find = |name: &str| fe.view.children(a).into_iter().find(|&c| fe.view.name(c) == name);
        match class.as_str() {
            "VaultCharacterPlacementHelper_C" => {
                let (Some(root), Some(place)) = (find("Root"), find("CharacterPlacement")) else { continue };
                let wi = WidgetCompInfo { widget_class: String::new(), screen_space: true, pivot: [0.5, 0.5], draw_size: [500., 500.], draw_at_desired_size: false };
                let pw = fe.world_xf(place);
                let character = Xf::from_pyr(pw.t.to_array(), [0.0, pw.rotator_deg()[1] + VAULT_PAWN_MESH_YAW, 0.0], [1.0; 3]);
                out.placements.push(PlacementInfo { actor: fe.view.name(a).to_owned(), player_id: 0, hidden: false, player_on_pad: false, root: fe.world_xf(root), character,
                    top_widget: Xf::IDENTITY, add_widget: Xf::IDENTITY, gadgets_widget: Xf::IDENTITY, top_info: wi.clone(), add_info: wi });
            }
            "VaultWeaponPlacementHelper_C" => { if let Some(place) = find("CharacterPlacement") { out.item_preview = Some(fe.world_xf(place)); } }
            "VaultCharacterLightingBP_C" => fe.collect_actor(a, &mut out),
            _ => {}
        }
    }
    if out.camera.as_ref().is_some_and(|c| c.actor_class.starts_with("FrontendCamera_Manage")) {
        let low_only = shadow_quality() == 0;
        for l in out.lights.iter_mut().filter(|l| l.source.contains("VaultCharacterLightingBP")) {
            l.visible = if l.component == "SkyLightLowSettings" { low_only } else { !low_only };
        }
    }
    if out.placements.is_empty() { return Err("no VaultCharacterPlacementHelper in the persistent level".into()); }
    fe.collect_post_volumes(&mut out);
    bg.collect_post_volumes(&mut out);
    out.post = stack_post_settings(&out.post_volumes, cam_pos);
    Ok(out)
}
/// Default values of a `MaterialParameterCollection` packed as its uniform buffer (`MaterialCollection0`): `fn_fx::mpc` (UE4.16 `GetDefaultParameterData`:
/// the scalar parameters four to a `float4` FIRST, then one `float4` per vector parameter; MATERIAL PARAMETERS 2026-10-06: this used to put the vectors first,
/// which only gave the right buffer because `MPC_Pinata` has no vector parameter). `M_StoreBGDarken` reads `BGChange` at `pcb0[0].x`, its first scalar.
pub fn read_mpc_defaults(set: &fn_pak::PakSet, long: &str) -> Result<Vec<[f32; 4]>, String> {
    let name = long.rsplit('/').next().unwrap_or(long);
    let c = fn_fx::mpc::Collection::load(set, &format!("{long}.{name}"))?;
    Ok(c.data(&fn_fx::mpc::Instance::default()))
}

/// The scene a front-end screen shows behind its widgets, for any `EFrontEndCamera` the screen requests with `FortFrontEndContext::SetFrontEndCamera`:
/// the camera actor `FrontEndSettingsBP_31.Cameras[camera]`, and every static / light / fog of the persistent level and of the always-loaded streaming levels
/// (`FrontEndStore`, `Frontend_BG_Main`, `Frontend_SkillTree`, with their `LevelTransform`) within `radius_cm` of that camera, plus the post-process volumes at the camera.
/// `camera` is the enumerator name (`"Store"`, `"StoreItemInspect"`, ...). Which levels contributed is listed in `unrendered` as `("source", level, "...")`.
pub fn read_view_scene_from(set: &fn_pak::PakSet, camera: &str, radius_cm: f32) -> Result<LobbySceneData, String> {
    let mut out = LobbySceneData::default();
    let fe = MapReader::open(set, FRONTEND_MAP, Vec3::ZERO)?;
    let streaming = read_frontend_streaming(&fe);
    let pairs = read_camera_pairs(&fe);
    let suffix = format!("::{camera}");
    let cam_actor = pairs.iter().find(|(t, _)| t.ends_with(&suffix)).map(|(_, a)| *a).ok_or_else(|| format!("FrontEndSettings maps no {camera} camera"))?;
    let ai = fe.actor_info(cam_actor);
    let cam_comp = fe.view.children(cam_actor).into_iter().find(|&c| fe.view.name(c) == "CameraComponent" && fe.view.class(c) == "CameraComponent").ok_or("camera actor has no CameraComponent")?;
    let cp = fe.comp_props(cam_comp);
    out.camera = Some(CameraInfo {
        actor: ai.name.clone(), actor_class: ai.class.clone(), xf: fe.world_xf(cam_comp), fov_deg: pv_f32(&cp, "FieldOfView").unwrap_or(90.), aspect_ratio: pv_f32(&cp, "AspectRatio").unwrap_or(1.777778),
        post_process_blend_weight: pv_f32(&cp, "PostProcessBlendWeight").unwrap_or(1.0), child_camera: None,
    });
    let cam_pos = out.camera.as_ref().unwrap().xf.t;
    let mut push_level = |out: &mut LobbySceneData, r: &MapReader, name: &str| {
        let mut scratch = LobbySceneData::default();
        for a in r.actors() {
            let class = r.view.class(a);
            if matches!(class.as_str(), "FortWorldSettings" | "LevelBounds" | "Level" | "WorldSettings") { continue; }
            // an actor belongs to the view when its root component is within the radius (the camera's own actor is included: its lights are bVisible=false templates)
            let Some(root) = r.view.export_of(r.view.props(a).get("RootComponent")) else { continue };
            if r.world_xf(root).t.distance(cam_pos) > radius_cm { continue; }
            r.collect_actor(a, &mut scratch);
        }
        let s = scratch.statics.into_iter().filter(|s| s.xf.t.distance(cam_pos) <= radius_cm).collect::<Vec<_>>();
        let l = scratch.lights.into_iter().filter(|l| l.xf.t.distance(cam_pos) <= radius_cm).collect::<Vec<_>>();
        out.unrendered.push(("source".into(), name.to_owned(), format!("{} statics, {} lights, {} fogs, {} particles within {radius_cm} cm of the camera", s.len(), l.len(), scratch.fogs.len(), scratch.particles.len())));
        out.statics.extend(s);
        out.lights.extend(l);
        out.fogs.extend(scratch.fogs);
        out.particles.extend(scratch.particles);
    };
    push_level(&mut out, &fe, FRONTEND_MAP);
    for (_, long, off) in streaming.iter().filter(|s| s.0.starts_with("LevelStreamingAlwaysLoaded")) {
        let r = MapReader::open(set, long, *off)?;
        push_level(&mut out, &r, long);
        r.collect_post_volumes(&mut out);
    }
    fe.collect_post_volumes(&mut out);
    out.post = stack_post_settings(&out.post_volumes, cam_pos);
    // `StoreCamera_Blueprint` (the actor of `EFrontEndCamera::Store`) calls `Mesh-DarkenBG.SetVisibility(true)` (ubergraph 0x3d2 / 0x489, `DarkenBGVisibility(Enable)`) and
    // `SetScalarParameterValue(MPC_Pinata, BGChange, 1)`: its template has `bVisible = false`, the activated camera shows the darkening plane (INFERENCE: these run when the camera activates).
    if camera == "Store" {
        for s in out.statics.iter_mut().filter(|s| s.component == "Mesh-DarkenBG") { s.visible = true; }
        // the Store materials read `MPC_Pinata` (`M_StoreBGDarken`: BGChange); its asset defaults are the values in effect (BGChange = 1 is also what the camera Kismet sets)
        out.mpc = read_mpc_defaults(set, "/Game/FrontEnd/Store/Pinata/Assets/Shared/MPC_Pinata")?;
    }
    Ok(out)
}
// __SCENE_DATA_4__

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn athena_lobby_root_slots_are_read_from_the_18_widget_tree() {
        let root = fn_live::var_os("FN_ROOT").map(std::path::PathBuf::from)
            .unwrap_or_else(|| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        if !root.exists() { eprintln!("1.8 install missing: skipped"); return; }
        let layout = read_athena_lobby_root_layout(&root).unwrap();
        assert_eq!(layout.season_padding, [32., 32., 0., 0.]);
        assert_eq!(layout.player_padding, [0., 0., 0., 134.]);
        assert_eq!(layout.launch_padding, [0., 0., 20., 15.]);
        assert!(layout.player_center_bottom);
        assert!(layout.launch_right_bottom);
    }

    #[test]
    fn athena_lobby_widget_tree_preserves_serialized_root_children() {
        let root = fn_live::var_os("FN_ROOT").map(std::path::PathBuf::from)
            .unwrap_or_else(|| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        if !root.exists() { eprintln!("1.8 install missing: skipped"); return; }
        let tree = read_athena_lobby_widget_tree(&root).unwrap();
        assert_eq!(tree.root, "SafeZone_0");
        let overlay = tree.widgets.iter().find(|w| w.name == "OverlayMain").unwrap();
        assert_eq!(overlay.class, "Overlay");
        assert_eq!(overlay.children, ["SeasonAndChallengesPanel", "PlayerPanel", "Launch", "ButtonGamepadSelect", "SizeBox_0"]);
        let player = tree.widgets.iter().find(|w| w.name == "PlayerPanel").unwrap();
        assert_eq!(player.class_path.as_deref(), Some("/Game/Athena/UI/Frontend/AthenaLobbyPlayerPanel.AthenaLobbyPlayerPanel_C"));
        let launch = tree.widgets.iter().find(|w| w.name == "Launch").unwrap();
        assert_eq!(launch.class_path.as_deref(), Some("/Game/Athena/UI/AthenaMatchmakingWidget.AthenaMatchmakingWidget_C"));
        assert!(tree.widgets.iter().any(|w| w.name == "Image_0"));
    }

    #[test]
    fn athena_lobby_child_trees_are_loaded_through_serialized_blueprint_paths() {
        let root = fn_live::var_os("FN_ROOT").map(std::path::PathBuf::from)
            .unwrap_or_else(|| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        if !root.exists() { eprintln!("1.8 install missing: skipped"); return; }
        let children = read_athena_lobby_child_widget_trees(&root).unwrap();
        let names: Vec<_> = children.iter().map(|(name, _)| name.as_str()).collect();
        assert!(names.contains(&"PlayerPanel"));
        assert!(names.contains(&"Launch"));
        assert!(names.contains(&"ButtonGamepadSelect"));
        assert!(children.iter().any(|(name, tree)| name == "Launch" && tree.widgets.iter().any(|widget| widget.name == "PlayButton")));
    }

    #[test]
    fn athena_lobby_graph_expands_real_player_and_matchmaking_widget_trees() {
        fn contains_node(node: &CookedWidgetNode, wanted: &str) -> bool {
            node.name == wanted || node.children.iter().any(|child| contains_node(child, wanted))
        }
        fn named<'a>(node: &'a CookedWidgetNode, wanted: &str) -> Option<&'a CookedWidgetNode> {
            if node.name == wanted { Some(node) } else { node.children.iter().find_map(|child| named(child, wanted)) }
        }

        let root = fn_live::var_os("FN_ROOT").map(std::path::PathBuf::from)
            .unwrap_or_else(|| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        if !root.exists() { eprintln!("1.8 install missing: skipped"); return; }
        let graph = read_athena_lobby_widget_graph(&root).unwrap();
        assert_eq!(graph.name, "SafeZone_0");
        let overlay = graph.children.iter().find(|node| node.name == "OverlayMain").unwrap();
        let player = overlay.children.iter().find(|node| node.name == "PlayerPanel").unwrap();
        assert_eq!(player.class_path.as_deref(), Some("/Game/Athena/UI/Frontend/AthenaLobbyPlayerPanel.AthenaLobbyPlayerPanel_C"));
        assert!(!player.children.is_empty(), "PlayerPanel must expand its child WidgetTree");
        let launch = overlay.children.iter().find(|node| node.name == "Launch").unwrap();
        assert!(!launch.children.is_empty(), "Launch must expand its child WidgetTree: {launch:#?}");
        assert!(contains_node(launch, "PlayButton"), "expanded Launch tree must contain the real PlayButton");
        let season = overlay.children.iter().find(|node| node.name == "SeasonAndChallengesPanel").unwrap();
        let size_box = named(season, "SizeBox_1").expect("serialized PlayerEmblemAndXp SizeBox");
        assert_eq!(size_box.width_override, Some(464.));
    }

    #[test]
    fn athena_tabs_are_owned_by_the_real_parent_screen_not_the_lobby_shell() {
        let root = fn_live::var_os("FN_ROOT").map(std::path::PathBuf::from)
            .unwrap_or_else(|| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        if !root.exists() { eprintln!("1.8 install missing: skipped"); return; }
        let tree = read_athena_tabs_widget_tree(&root).unwrap();
        assert_eq!(tree.root, "VerticalBox_1");
        let tabs = tree.widgets.iter().find(|widget| widget.name == "TopTabList").unwrap();
        assert_eq!(tabs.class, "HorizontalTabList_C");
        assert!(tabs.children.is_empty(), "the UMG class template owns the tab buttons");
    }

    #[test]
    fn athena_tab_registrations_keep_the_cooked_labels_routes_and_visibility() {
        let root = fn_live::var_os("FN_ROOT").map(std::path::PathBuf::from)
            .unwrap_or_else(|| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        if !root.exists() { eprintln!("1.8 install missing: skipped"); return; }
        let tabs = read_athena_tab_registrations(&root).unwrap();
        let visible: Vec<_> = tabs.iter().filter(|tab| !tab.hidden).map(|tab| tab.label.as_str()).collect();
        assert_eq!(visible, ["Lobby", "Locker", "Item Shop", "Season Shop", "Stats", "Leaderboards", "Store"]);
        let lobby = tabs.iter().find(|tab| tab.id == "Lobby").unwrap();
        assert_eq!(lobby.content_class.as_deref(), Some("/Game/Athena/UI/Frontend/AthenaLobby.AthenaLobby_C"));
    }

    #[test]
    fn frontend_top_bar_tree_contains_the_real_currency_and_social_widgets() {
        let root = fn_live::var_os("FN_ROOT").map(std::path::PathBuf::from)
            .unwrap_or_else(|| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        if !root.exists() { eprintln!("1.8 install missing: skipped"); return; }
        let tree = read_widget_tree(&root, FRONTEND_TOP_BAR_WIDGET, "WidgetTree_475").unwrap();
        assert_eq!(tree.root, "Overlay_1");
        for (name, class) in [("FortniteLogo", "Image"), ("BRCurrency", "BRCurrency_C"), ("Social", "IconTextButton_C"), ("MiniPartyBar", "MiniPartyBar_C")] {
            assert!(tree.widgets.iter().any(|w| w.name == name && w.class == class), "missing {name}:{class}");
        }
    }

    #[test]
    fn frontend_top_bar_visuals_use_the_serialized_logo_and_social_brushes() {
        let root = fn_live::var_os("FN_ROOT").map(std::path::PathBuf::from)
            .unwrap_or_else(|| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        if !root.exists() { eprintln!("1.8 install missing: skipped"); return; }
        let visuals = read_widget_visuals(&root, FRONTEND_TOP_BAR_WIDGET, "WidgetTree_475").unwrap();
        let logo = visuals.iter().find(|v| v.name == "FortniteLogo").unwrap();
        assert_eq!(logo.brush_size, Some([204., 64.]));
        assert_eq!(logo.brush_resource.as_deref(), Some("/Game/UI/Foundation/Textures/Logo/T-Logo-Wordmark-1024.T-Logo-Wordmark-1024"));
        let social = visuals.iter().find(|v| v.name == "Social").unwrap();
        assert_eq!(social.brush_resource.as_deref(), Some("/Game/UI/Foundation/Textures/Icons/Social/T-Icon-Chat-Party-XL.T-Icon-Chat-Party-XL"));
    }

    #[test]
    fn athena_play_button_uses_the_serialized_primary_large_style() {
        let root = fn_live::var_os("FN_ROOT").map(std::path::PathBuf::from)
            .unwrap_or_else(|| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        if !root.exists() { eprintln!("1.8 install missing: skipped"); return; }
        let style = read_button_style(&root, "/Game/UI/Foundation/Buttons/ButtonStyles/Primary/ButtonStyle-Primary-L.ButtonStyle-Primary-L_C").unwrap();
        assert_eq!(style.min_size, [192., 56.]);
        assert_eq!(style.normal.resource, "/Game/UI/Foundation/Buttons/Materials/Primary/M-Button-Primary-L-Base.M-Button-Primary-L-Base");
        assert_eq!(style.hovered.resource, "/Game/UI/Foundation/Buttons/Materials/Primary/M-Button-Primary-L-Hovered.M-Button-Primary-L-Hovered");
        assert_eq!(style.pressed.resource, "/Game/UI/Foundation/Buttons/Materials/Primary/M-Button-Primary-L-Pressed.M-Button-Primary-L-Pressed");
        assert_eq!(style.selected.resource, style.pressed.resource);
        assert_eq!(style.normal.draw_as, "ESlateBrushDrawType::Box");
        assert_eq!(style.normal.margin, [0.25, 0.5, 0.25, 0.5]);
    }

    // ---------------------------------------------------------------- scene data: every value below is the literal serialized value of the 1.8 package
    // (decoded independently with `fn-inspect props` while auditing, see parity/lobby/SCENE.md)
    fn install() -> Option<std::path::PathBuf> {
        let root = fn_live::var_os("FN_ROOT").map(std::path::PathBuf::from).unwrap_or_else(|| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        if root.exists() { Some(root) } else { eprintln!("1.8 install missing: skipped"); None }
    }
    fn scene() -> Option<LobbySceneData> { Some(read_lobby_scene(&install()?).expect("lobby scene")) }
    #[test]
    fn halloween_matinee_drives_the_four_ghost_actors() {
        let Some(d) = scene() else { return };
        let m = d.matinee.as_ref().expect("MatineeActor_Halloween");
        assert_eq!(m.actor, "MatineeActor_Halloween");
        assert!(m.looping && m.play_on_load && near(m.length, 120.24581));
        let names: Vec<&str> = m.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["TrollGhost", "ghostroll2", "ghosttroll3", "Taker"]);
        for g in &m.groups { assert!(d.skeletals.iter().any(|k| k.actor == g.actor), "{} -> {} is not a skeletal actor", g.name, g.actor); }
        let troll = &m.groups[0];
        let (p, _) = troll.transform(0.05).unwrap();
        assert!(p.distance(Vec3::new(-193.10884, 125163.5, 4.005684)) < 1e-2);
        assert!(troll.visible(1.0, true, false) && !troll.visible(9.2, true, false));
        assert_eq!(troll.anims[0].seq, "/Game/Characters/Enemies/Thief/Animation/jump_out.jump_out");
        assert!(m.groups[3].visible(100.0, false, true));
        assert!(!d.missing.iter().any(|x| x.contains("Matinee")), "{:?}", d.missing);
    }
    fn near(a: f32, b: f32) -> bool { (a - b).abs() < 1e-3 * b.abs().max(1.) }

    #[test]
    fn xf_composition_and_rotator_match_ue() {
        let q = rotator_quat([0., 90., 0.]);
        assert!((q * Vec3::X - Vec3::Y).length() < 1e-6, "yaw +90 turns +X into +Y");
        let parent = Xf::from_pyr([10., 20., 30.], [0., 90., 0.], [2., 2., 2.]);
        let child = Xf::from_pyr([1., 0., 0.], [0., 0., 0.], [1., 1., 1.]);
        let w = child.compose(&parent);
        assert!((w.t - Vec3::new(10., 22., 30.)).length() < 1e-5, "{:?}", w.t);
        let r = Xf::from_pyr([0.; 3], [-4.4, -90.2, 7.0], [1.; 3]).rotator_deg();
        assert!((r[0] + 4.4).abs() < 1e-3 && (r[1] + 90.2).abs() < 1e-3 && (r[2] - 7.0).abs() < 1e-3, "{r:?}");
    }

    #[test]
    fn blackbody_6500k_is_near_white_and_12000k_is_blue() {
        let w = blackbody_linear(6500.);
        assert!((w[0] - 1.).abs() < 0.08 && (w[2] - 1.).abs() < 0.12, "{w:?}");
        let b = blackbody_linear(12000.);
        assert!(b[2] > b[0], "{b:?}");
    }

    #[test]
    fn the_front_end_picks_the_halloween_lobby_and_the_hero_select_camera_is_frontend_lobby() {
        let Some(d) = scene() else { return };
        let sel = d.selection.as_ref().unwrap();
        assert_eq!(sel.lobby_select, "FortniteLobby::NewEnumerator1");
        assert_eq!(sel.lobby_select_label, "Halloween");
        assert_eq!(sel.map, HALLOWEEN_MAP);
        let off = |m: &str| sel.frontend_streaming.iter().find(|s| s.1 == m).map(|s| s.2).unwrap();
        assert_eq!(off(HALLOWEEN_MAP), Vec3::ZERO);
        assert_eq!(off(LOBBY_SELECTOR_MAP), Vec3::new(0., 125397.5625, 0.));
        for ty in ["EFrontEndCamera::HeroSelect", "EFrontEndCamera::Party"] {
            assert!(sel.camera_pairs.iter().any(|(t, a)| t == ty && a == "Frontend_Lobby"), "{ty}");
        }
    }

    #[test]
    fn camera_is_the_frontend_lobby_actor_root_camera() {
        let Some(d) = scene() else { return };
        let c = d.camera.as_ref().unwrap();
        assert_eq!((c.actor.as_str(), c.actor_class.as_str()), ("Frontend_Lobby", "FortnitePartyBackdrop_Camera_C"));
        assert!((c.xf.t - Vec3::new(-10.0, 125992.5, 90.4527587890625)).length() < 1e-3);
        let r = c.xf.rotator_deg();
        assert!(r[0].abs() < 1e-3 && (r[1] + 90.).abs() < 1e-3 && r[2].abs() < 1e-3, "{r:?}");
        assert_eq!(c.fov_deg, 55.0, "Default__FortnitePartyBackdrop_Camera_C CameraComponent FieldOfView");
        assert_eq!(c.post_process_blend_weight, -1.0);
        let (cx, cf) = c.child_camera.unwrap();
        assert_eq!(cf, 35.0, "CameraComponent_0 template");
        let want = c.xf.t + c.xf.q * Vec3::new(-541.2623901367188, -41.0029296875, 133.9656982421875);
        assert!((cx.t - want).length() < 0.05, "{:?} vs {want:?}", cx.t);
    }

    #[test]
    fn party_slots_match_the_package_values() {
        let Some(d) = scene() else { return };
        assert_eq!(d.placements.len(), 5);
        let p0 = &d.placements[0];
        assert!((p0.root.t - Vec3::new(-6.0546875, 125412.96875, 2.939453125)).length() < 1e-2);
        assert!(near(p0.root.rotator_deg()[1], 89.99995422363281));
        // Player0 overrides CharacterPlacement to (0, 1.5e-5, 0) / yaw -90.0000076: same point as the pad root, world yaw 0
        assert!((p0.character.t - p0.root.t).length() < 1e-3);
        assert!(p0.character.rotator_deg()[1].abs() < 1e-3);
        assert!(p0.player_on_pad && !p0.hidden);
        let p1 = &d.placements[1];
        assert!((p1.root.t - Vec3::new(126.50390625, 125310.96875, 3.939453125)).length() < 1e-2);
        assert!(near(p1.root.rotator_deg()[1], 100.00008392333984));
        // template CharacterPlacement (StaticMeshComponent_231): (20, 0, 0), yaw -90
        let want = p1.root.t + p1.root.q * Vec3::new(20., 0., 0.);
        assert!((p1.character.t - want).length() < 1e-2, "{:?} vs {want:?}", p1.character.t);
        assert!((p1.character.rotator_deg()[1] - 10.00008).abs() < 1e-2);
        assert!(d.placements[4].hidden && !d.placements[1].hidden && !d.placements[3].hidden);
        assert!((d.placements[2].root.t - Vec3::new(-126.0546875, 125302.96875, 3.939453125)).length() < 1e-2);
        assert!((d.placements[3].root.t - Vec3::new(257.9453125, 125198.96875, 3.939453125)).length() < 1e-2);
        assert!((p0.top_widget.t - (p0.root.t + Vec3::new(0., 0., 165.))).length() < 1e-2, "LobbyPlayerPadTop RelativeLocation z=165 on Player0");
        // WidgetComponent data of the pad blueprint templates (LobbyPlayerPadTop_GEN_VARIABLE / LobbyPlayerAddPlayer_GEN_VARIABLE)
        for p in &d.placements {
            assert!(p.top_info.screen_space && p.add_info.screen_space && p.top_info.draw_at_desired_size && p.add_info.draw_at_desired_size);
            assert_eq!(p.top_info.pivot, [0.5, 1.0], "name plate pivot: bottom centre on the projected point");
            assert_eq!(p.add_info.pivot, [0.5, 0.5], "+ marker: ctor default pivot (not serialized)");
            assert!(p.top_info.widget_class.ends_with("LobbyPlayerPadTop.LobbyPlayerPadTop_C") && p.add_info.widget_class.ends_with("LobbyPlayerAddPlayer.LobbyPlayerAddPlayer_C"));
        }
    }

    #[test]
    fn statics_include_the_matte_plane_terrain_foliage_and_pads() {
        let Some(d) = scene() else { return };
        let matte: Vec<_> = d.statics.iter().filter(|s| s.material_overrides.iter().flatten().any(|m| m.ends_with("M_LobbyMatte.M_LobbyMatte"))).collect();
        assert_eq!(matte.len(), 1);
        let m = matte[0];
        assert!(m.mesh.ends_with("TechContent/S_plane_01.S_plane_01"));
        assert_eq!(m.xf.t, Vec3::new(-721.65380859375, 121141.90625, 585.1259765625));
        assert!(near(m.xf.s.x, 15.396615028381348) && near(m.xf.s.z, 15.396615028381348));
        assert!((m.xf.rotator_deg()[0] + 2.2536754608154297).abs() < 1e-3);
        assert_eq!(d.statics.iter().filter(|s| s.mesh.ends_with("SM_Terrain_HalloweenLobby.SM_Terrain_HalloweenLobby")).count(), 8);
        assert_eq!(d.statics.iter().filter(|s| s.component.starts_with("FoliageInstancedStaticMeshComponent_0[")).count(), 380, "NumBuiltInstances");
        assert!(d.statics.iter().filter(|s| s.mesh.ends_with("SM_Lobby_Character_Pad.SM_Lobby_Character_Pad")).count() == 5);
        // the hidden CharacterPlacement helper mesh is kept but not visible
        assert!(d.statics.iter().filter(|s| s.mesh.ends_with("S_FE_CharacterPlacementHelper.S_FE_CharacterPlacementHelper")).all(|s| !s.visible));
        assert!(d.missing.iter().all(|m| !m.contains("instances not placed")), "{:?}", d.missing);
    }

    #[test]
    fn lights_fog_and_post_match_the_package_values() {
        let Some(d) = scene() else { return };
        let sun = d.lights.iter().find(|l| l.actor == "DirectionalLight_1").unwrap();
        assert!(sun.visible && sun.use_temperature && sun.temperature == 12000.0);
        assert!(near(sun.intensity, 2.7522940635681152));
        let r = sun.xf.rotator_deg();
        assert!(near(r[0], -11.517631530761719) && near(r[1], 101.17805480957031), "{r:?}");
        assert!(!d.lights.iter().find(|l| l.actor == "DirectionalLight_LOWquality").unwrap().visible, "shadow quality != 0: the low-detail light stays off");
        let sky = d.lights.iter().find(|l| l.kind == LightKind::Sky && l.actor == "SkyLight_1").unwrap();
        assert!(near(sky.intensity, 0.7339450120925903) && sky.cubemap.as_deref() == Some("/Engine/MapTemplates/daylight.daylight"));
        // pad lights: only Player0's TopSpotLight is visible (bVisible serialized true on that instance)
        let tops: Vec<_> = d.lights.iter().filter(|l| l.component == "TopSpotLight").collect();
        assert_eq!(tops.len(), 5);
        assert!(tops.iter().all(|l| l.visible == (l.actor == "Player0")));
        let t = tops.iter().find(|l| l.actor == "Player0").unwrap();
        assert_eq!((t.intensity, t.attenuation_radius, t.inner_cone_deg, t.outer_cone_deg), (18000.0, 500.0, 12.0, 20.0), "TopSpotLight template");
        let fog = d.fogs.iter().find(|f| f.visible).unwrap();
        assert!(near(fog.density, 0.017431000247597694) && fog.height_falloff == 2.0 && fog.volumetric);
        // post: the priority 200 volume overrides the priority 100 one; the LUT / blendables come from the lobby level volume
        assert!(near(d.post.f1("FilmSlope"), 0.9174309968948364) && near(d.post.f1("FilmToe"), 0.42844000458717346));
        assert_eq!((d.post.f1("AutoExposureMinBrightness"), d.post.f1("AutoExposureMaxBrightness")), (1.0, 1.0));
        assert!(near(d.post.f1("VignetteIntensity"), 0.583486020565033));
        assert_eq!(d.post.color_grading_lut.as_deref(), Some("/Game/Environments/World/LUT/T_LUT_Vibrant02.T_LUT_Vibrant02"));
        assert!(d.post.blendables.iter().any(|b| b.0.ends_with("M_Lobby_UI.M_Lobby_UI")));
        let sat = d.post.v4("ColorSaturationMidtones");
        assert!(near(sat[0], 1.149999976158142) && near(sat[1], 0.9200000166893005));
    }
}
