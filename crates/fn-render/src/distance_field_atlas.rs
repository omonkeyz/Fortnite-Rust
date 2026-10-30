//! DF ATLAS (2026-10-06, freezes): the allocation of the mesh distance-field volume atlas as UE4.16 does it.
//!
//! * [`TextureLayout3d`] = `FTextureLayout3d` (Engine/Public/TextureLayout3d.h): the kd-tree packer, created by the atlas with `bPowerOfTwoSize` false and
//!   `bAlignByFour` false in a `r.DistanceFields.AtlasSizeXY` x XY x `r.DistanceFields.AtlasSizeZ` (512 x 512 x 1024) space (DistanceFieldAtlas.cpp:124, :287).
//!   `AddElement` first tries to place an element without enlarging the used size, then with; the used size grows to the placed extents.
//! * [`VolumeAtlas`] = `FDistanceFieldVolumeTextureAtlas::UpdateAllocations` (DistanceFieldAtlas.cpp:246-436): the pending volumes, sorted largest first
//!   (`FCompareVolumeAllocation`), are added to the layout. When the layout has outgrown the texture (or there is none) and volumes were already placed,
//!   the layout is rebuilt from scratch with every volume (largest first), `Generation` is bumped and the whole atlas is uploaded again into a texture of
//!   the new layout size; otherwise only the new blocks are uploaded (`RHIUpdateTexture3D` per allocation).
//!
//! The placement of a block never changes a sampled value (the traces clamp to the block's texel centres), so this only decides how much is uploaded
//! when a volume is added and when the dependent data (object atlas coordinates, the global distance field) has to be rebuilt: on a generation change.

/// `FTextureLayoutNode3d`.
#[derive(Clone, Copy, Debug)]
struct Node { child_a: Option<usize>, child_b: Option<usize>, min: [u32; 3], size: [u32; 3], used: bool }
impl Node { fn new(min: [u32; 3], size: [u32; 3]) -> Node { Node { child_a: None, child_b: None, min, size, used: false } } }

/// `FTextureLayout3d` with `bPowerOfTwoSize` = `bAlignByFour` = false (the atlas' settings).
#[derive(Clone, Debug)]
pub struct TextureLayout3d { size: [u32; 3], nodes: Vec<Node> }

impl TextureLayout3d {
    /// `FTextureLayout3d(MinSizeX, MinSizeY, MinSizeZ, MaxSizeX, MaxSizeY, MaxSizeZ)`.
    pub fn new(min_size: [u32; 3], max_size: [u32; 3]) -> TextureLayout3d { TextureLayout3d { size: min_size, nodes: vec![Node::new([0; 3], max_size)] } }
    /// The used size (`GetSizeX/Y/Z`).
    pub fn size(&self) -> [u32; 3] { self.size }
    /// `AddElement`: the base of a free `element`-sized area, or `None` when the layout is full.
    pub fn add_element(&mut self, element: [u32; 3]) -> Option<[u32; 3]> {
        if element.iter().any(|&e| e == 0) { return Some([0; 3]); }
        let node = self.add_surface_inner(0, element, false).or_else(|| self.add_surface_inner(0, element, true))?;
        let n = &mut self.nodes[node];
        n.used = true;
        let min = n.min;
        for a in 0..3 { self.size[a] = self.size[a].max(min[a] + element[a]); }
        Some(min)
    }
    /// `AddSurfaceInner`. The enlargement check compares the Z extent with the element's Y size, as UE4.16 does (TextureLayout3d.h:267).
    fn add_surface_inner(&mut self, index: usize, e: [u32; 3], allow_enlargement: bool) -> Option<usize> {
        let node = self.nodes[index];
        if let (Some(a), Some(b)) = (node.child_a, node.child_b) {
            return self.add_surface_inner(a, e, allow_enlargement).or_else(|| self.add_surface_inner(b, e, allow_enlargement));
        }
        if node.used { return None; }
        if node.size[0] < e[0] || node.size[1] < e[1] || node.size[2] < e[2] { return None; }
        if !allow_enlargement && (node.min[0] + e[0] > self.size[0] || node.min[1] + e[1] > self.size[1] || node.min[2] + e[1] > self.size[2]) { return None; }
        if node.size == e { return Some(index); }
        let excess = [node.size[0] - e[0], node.size[1] - e[1], node.size[2] - e[2]];
        let (min, size) = (node.min, node.size);
        // split along the axis with the most excess (X before Y before Z on ties, as the nested comparisons of UE4.16 do)
        let axis = if excess[0] > excess[1] { if excess[0] > excess[2] { 0 } else { 2 } } else if excess[1] > excess[2] { 1 } else { 2 };
        let mut a_size = size;
        a_size[axis] = e[axis];
        let mut b_min = min;
        b_min[axis] += e[axis];
        let mut b_size = size;
        b_size[axis] -= e[axis];
        let a = self.nodes.len();
        self.nodes.push(Node::new(min, a_size));
        self.nodes.push(Node::new(b_min, b_size));
        self.nodes[index].child_a = Some(a);
        self.nodes[index].child_b = Some(a + 1);
        // only ChildA is traversed: it is the newly created node that matches the element size along the split axis
        self.add_surface_inner(a, e, allow_enlargement)
    }
}

/// What [`VolumeAtlas::update`] decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtlasUpdate<K> {
    /// Nothing pending.
    None,
    /// These volumes were added inside the current texture: upload only their blocks.
    Blocks(Vec<K>),
    /// A new texture of [`VolumeAtlas::texture_size`]: upload every placed volume. `regenerated` = volumes were placed before (`Generation` bumped: every
    /// dependent atlas coordinate changed).
    Full { regenerated: bool },
}

/// `FDistanceFieldVolumeTextureAtlas` allocation state for volumes keyed by `K`.
#[derive(Clone, Debug)]
pub struct VolumeAtlas<K> {
    max_size: [u32; 3],
    layout: TextureLayout3d,
    /// (key, size, block min)
    current: Vec<(K, [u32; 3], [u32; 3])>,
    pending: Vec<(K, [u32; 3])>,
    texture_size: Option<[u32; 3]>,
    pub generation: u32,
    /// Volumes `AddElement` could not place ("Failed to allocate ... in distance field atlas").
    pub failed: Vec<K>,
    previous: Vec<(K, [u32; 3])>,
    pad: Option<[u32; 3]>,
}

impl<K: Copy + PartialEq + std::fmt::Debug> VolumeAtlas<K> {
    pub fn new(max_size: [u32; 3]) -> VolumeAtlas<K> {
        VolumeAtlas { max_size, layout: TextureLayout3d::new([0; 3], max_size), current: vec![], pending: vec![], texture_size: None, generation: 0, failed: vec![], previous: vec![], pad: None }
    }
    pub fn with_texture_padding(mut self, step: [u32; 3]) -> VolumeAtlas<K> { self.pad = Some(step); self }
    /// `AddAllocation`.
    pub fn add(&mut self, key: K, size: [u32; 3]) { if !self.current.iter().any(|c| c.0 == key) && !self.pending.iter().any(|p| p.0 == key) { self.pending.push((key, size)); } }
    /// `RemoveAllocation` of a placed volume: UE frees its layout node; here every remaining volume is placed again on the next [`Self::update`] (a
    /// generation change). Only a replaced mesh volume does this.
    pub fn remove(&mut self, key: K) {
        self.pending.retain(|p| p.0 != key);
        if let Some(i) = self.current.iter().position(|c| c.0 == key) {
            self.current.remove(i);
            let keep: Vec<(K, [u32; 3])> = self.current.drain(..).map(|(k, s, _)| (k, s)).collect();
            self.pending.extend(keep);
            self.layout = TextureLayout3d::new([0; 3], self.max_size);
            self.texture_size = None;
            self.generation += 1;
        }
    }
    pub fn has_pending(&self) -> bool { !self.pending.is_empty() }
    /// The size of the atlas texture (the layout size when it was last (re)created).
    pub fn texture_size(&self) -> Option<[u32; 3]> { self.texture_size }
    /// The block min of a placed volume.
    pub fn block(&self, key: K) -> Option<[u32; 3]> { self.current.iter().find(|c| c.0 == key).map(|c| c.2) }
    /// Every placed volume: (key, size, block min).
    pub fn placed(&self) -> &[(K, [u32; 3], [u32; 3])] { &self.current }
    pub fn take_previous(&mut self) -> Vec<(K, [u32; 3])> { std::mem::take(&mut self.previous) }
    /// `UpdateAllocations`.
    pub fn update(&mut self) -> AtlasUpdate<K> {
        if self.pending.is_empty() { return AtlasUpdate::None; }
        // FCompareVolumeAllocation: largest volume first (a stable sort keeps the submission order of equal volumes)
        let vol = |s: &[u32; 3]| s[0] as u64 * s[1] as u64 * s[2] as u64;
        self.pending.sort_by(|a, b| vol(&b.1).cmp(&vol(&a.1)));
        let mut placed: Vec<(K, [u32; 3], [u32; 3])> = vec![];
        for (k, s) in std::mem::take(&mut self.pending) {
            match self.layout.add_element(s) { Some(min) => placed.push((k, s, min)), None => self.failed.push(k) }
        }
        let grown = match self.texture_size { None => true, Some(t) => (0..3).any(|a| self.layout.size()[a] > t[a]) };
        if !grown {
            let keys = placed.iter().map(|p| p.0).collect();
            self.current.extend(placed);
            return AtlasUpdate::Blocks(keys);
        }
        let regenerated = !self.current.is_empty();
        if regenerated {
            // a clean slate: every volume placed again, largest first
            self.layout = TextureLayout3d::new([0; 3], self.max_size);
            self.generation += 1;
            self.previous = self.current.iter().map(|c| (c.0, c.2)).collect();
            let mut all: Vec<(K, [u32; 3])> = self.current.drain(..).map(|(k, s, _)| (k, s)).chain(placed.into_iter().map(|(k, s, _)| (k, s))).collect();
            all.sort_by(|a, b| vol(&b.1).cmp(&vol(&a.1)));
            placed = vec![];
            for (k, s) in all { match self.layout.add_element(s) { Some(min) => placed.push((k, s, min)), None => self.failed.push(k) } }
        }
        self.current.extend(placed);
        let size = self.layout.size().map(|v| v.max(1));
        self.texture_size = Some(match self.pad { Some(p) => [0, 1, 2].map(|a| size[a].div_ceil(p[a].max(1)).saturating_mul(p[a].max(1)).min(self.max_size[a]).max(size[a])), None => size });
        AtlasUpdate::Full { regenerated }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_places_without_overlap_and_grows_only_when_needed() {
        let mut l = TextureLayout3d::new([0; 3], [512, 512, 1024]);
        let sizes = [[40, 40, 40], [40, 40, 40], [20, 30, 10], [100, 20, 64], [8, 8, 8], [40, 40, 40]];
        let mut boxes: Vec<([u32; 3], [u32; 3])> = vec![];
        for s in sizes {
            let m = l.add_element(s).expect("fits");
            for (bm, bs) in &boxes { assert!((0..3).any(|a| m[a] + s[a] <= bm[a] || bm[a] + bs[a] <= m[a]), "overlap {m:?}+{s:?} with {bm:?}+{bs:?}"); }
            for a in 0..3 { assert!(m[a] + s[a] <= l.size()[a]); }
            boxes.push((m, s));
        }
        // the first element sits at the origin and sets the size
        assert_eq!(boxes[0].0, [0, 0, 0]);
        // an element that fits in the used size does not enlarge it
        let before = l.size();
        let m = l.add_element([4, 4, 4]).unwrap();
        assert!((0..3).all(|a| m[a] + 4 <= before[a]) || l.size() != before);
    }

    #[test]
    fn atlas_uploads_new_blocks_in_place_and_regenerates_on_growth() {
        let mut at: VolumeAtlas<u32> = VolumeAtlas::new([512, 512, 1024]);
        at.add(0, [64, 64, 64]);
        at.add(1, [32, 32, 32]);
        assert_eq!(at.update(), AtlasUpdate::Full { regenerated: false });
        let t = at.texture_size().unwrap();
        assert_eq!(at.generation, 0);
        // a small volume fits inside the current texture: only its block is uploaded, nothing else moves
        let b0 = at.block(0);
        at.add(2, [16, 16, 16]);
        assert_eq!(at.update(), AtlasUpdate::Blocks(vec![2]));
        assert_eq!(at.texture_size(), Some(t));
        assert_eq!(at.block(0), b0);
        assert_eq!(at.generation, 0);
        // a volume that needs a larger texture: a new generation, everything placed again
        at.add(3, [128, 128, 128]);
        assert_eq!(at.update(), AtlasUpdate::Full { regenerated: true });
        assert_eq!(at.generation, 1);
        assert_eq!(at.placed().len(), 4);
        // already placed keys are not added twice
        at.add(3, [128, 128, 128]);
        assert_eq!(at.update(), AtlasUpdate::None);
    }
}
