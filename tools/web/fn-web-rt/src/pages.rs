pub const SHIFT: usize = 14;
pub const UNIT: usize = 1 << SHIFT;
const NBINS: usize = 88;

pub trait Grow {
    fn end(&self) -> usize;
    fn grow(&mut self, units: usize) -> Option<usize>;
    fn note(&mut self, _want: usize, _free: usize, _largest: usize) {}
}

pub struct Pages<const N: usize> {
    tag: [u32; N],
    head: [u32; NBINS],
    mask: [u64; 2],
    base: usize,
    free_units: usize,
}

#[inline]
pub fn units(bytes: usize) -> usize { bytes.div_ceil(UNIT) }

#[inline]
fn bin(n: usize) -> usize {
    if n <= 32 { return n - 1; }
    let b = (usize::BITS - 1 - n.leading_zeros()) as usize;
    (32 + (b - 5) * 4 + ((n >> (b - 2)) & 3)).min(NBINS - 1)
}

impl<const N: usize> Pages<N> {
    pub const fn new(base: usize) -> Self { Pages { tag: [0; N], head: [0; NBINS], mask: [0; 2], base, free_units: 0 } }

    pub fn free_units(&self) -> usize { self.free_units }

    pub fn largest_free(&self) -> usize {
        let mut b = NBINS;
        while b > 0 {
            b -= 1;
            if self.mask[b >> 6] & (1 << (b & 63)) == 0 { continue; }
            let mut best = 0;
            let mut cur = self.head[b];
            while cur != 0 { let s = cur as usize - 1; best = best.max(self.tag[s] as usize); cur = unsafe { *self.link(s) }; }
            return best;
        }
        0
    }

    #[inline]
    fn link(&self, u: usize) -> *mut u32 { (self.base + (u << SHIFT)) as *mut u32 }

    unsafe fn push(&mut self, s: usize, n: usize) {
        self.tag[s] = n as u32;
        self.tag[s + n - 1] = n as u32;
        let b = bin(n);
        let h = self.head[b];
        let p = self.link(s);
        unsafe {
            *p = h;
            *p.add(1) = 0;
            if h != 0 { *self.link(h as usize - 1).add(1) = s as u32 + 1; }
        }
        self.head[b] = s as u32 + 1;
        self.mask[b >> 6] |= 1 << (b & 63);
        self.free_units += n;
    }

    unsafe fn unlink(&mut self, s: usize, n: usize) {
        let b = bin(n);
        let p = self.link(s);
        let (nx, pv) = unsafe { (*p, *p.add(1)) };
        if pv != 0 { unsafe { *self.link(pv as usize - 1) = nx; } } else {
            self.head[b] = nx;
            if nx == 0 { self.mask[b >> 6] &= !(1 << (b & 63)); }
        }
        if nx != 0 { unsafe { *self.link(nx as usize - 1).add(1) = pv; } }
        self.tag[s] = 0;
        self.tag[s + n - 1] = 0;
        self.free_units -= n;
    }

    unsafe fn best_in(&self, b: usize, n: usize) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize)> = None;
        let mut cur = self.head[b];
        while cur != 0 {
            let s = cur as usize - 1;
            let len = self.tag[s] as usize;
            if len >= n && best.map_or(true, |(bs, bl)| len < bl || (len == bl && s < bs)) {
                best = Some((s, len));
                if len == n && b < 32 { break; }
            }
            cur = unsafe { *self.link(s) };
        }
        best
    }

    unsafe fn find(&self, n: usize) -> Option<(usize, usize)> {
        let b0 = bin(n);
        if let Some(r) = unsafe { self.best_in(b0, n) } { return Some(r); }
        let mut b = b0 + 1;
        while b < NBINS {
            let w = self.mask[b >> 6] >> (b & 63);
            if w == 0 { b = (b | 63) + 1; continue; }
            b += w.trailing_zeros() as usize;
            if b >= NBINS { break; }
            if let Some(r) = unsafe { self.best_in(b, n) } { return Some(r); }
            b += 1;
        }
        None
    }

    unsafe fn take(&mut self, s: usize, len: usize, n: usize) -> usize {
        if len > n { unsafe { self.push(s + n, len - n) }; }
        s
    }

    pub unsafe fn alloc(&mut self, n: usize, g: &mut impl Grow) -> Option<usize> {
        if n == 0 || n > N { return None; }
        if let Some((s, len)) = unsafe { self.find(n) } {
            unsafe { self.unlink(s, len); }
            return Some(unsafe { self.take(s, len, n) });
        }
        let end = g.end();
        if end > N { return None; }
        let tail = if end > 0 { self.tag[end - 1] as usize } else { 0 };
        let s = end - tail;
        if s + n > N { return None; }
        g.note(n, self.free_units, self.largest_free());
        let got = g.grow(n - tail)?;
        if tail > 0 { unsafe { self.unlink(s, tail); } }
        let len = (tail + got).min(N - s);
        Some(unsafe { self.take(s, len, n) })
    }

    pub unsafe fn alloc_low(&mut self, n: usize, g: &mut impl Grow) -> Option<usize> {
        if n == 0 || n > N { return None; }
        let mut best: Option<(usize, usize)> = None;
        let mut b = bin(n);
        while b < NBINS {
            let w = self.mask[b >> 6] >> (b & 63);
            if w == 0 { b = (b | 63) + 1; continue; }
            b += w.trailing_zeros() as usize;
            if b >= NBINS { break; }
            let mut cur = self.head[b];
            while cur != 0 {
                let s = cur as usize - 1;
                let len = self.tag[s] as usize;
                if len >= n && best.map_or(true, |(bs, _)| s < bs) { best = Some((s, len)); }
                cur = unsafe { *self.link(s) };
            }
            b += 1;
        }
        if let Some((s, len)) = best {
            unsafe { self.unlink(s, len); }
            return Some(unsafe { self.take(s, len, n) });
        }
        unsafe { self.alloc(n, g) }
    }

    pub unsafe fn free(&mut self, s: usize, n: usize) {
        if n == 0 { return; }
        let (mut s, mut n) = (s, n);
        if s > 0 {
            let pl = self.tag[s - 1] as usize;
            if pl != 0 { let ps = s - pl; unsafe { self.unlink(ps, pl); } s = ps; n += pl; }
        }
        if s + n < N {
            let nl = self.tag[s + n] as usize;
            if nl != 0 { unsafe { self.unlink(s + n, nl); } n += nl; }
        }
        unsafe { self.push(s, n); }
    }

    pub unsafe fn grow_in_place(&mut self, s: usize, n: usize, n2: usize, g: &mut impl Grow) -> bool {
        if n2 <= n { return true; }
        let need = n2 - n;
        let nx = s + n;
        if s + n2 > N { return false; }
        let nl = if nx < N { self.tag[nx] as usize } else { 0 };
        if nl >= need {
            unsafe { self.unlink(nx, nl); }
            if nl > need { unsafe { self.push(s + n2, nl - need); } }
            return true;
        }
        if nx + nl != g.end() { return false; }
        g.note(need, self.free_units, self.largest_free());
        let Some(got) = g.grow(need - nl) else { return false };
        if nl > 0 { unsafe { self.unlink(nx, nl); } }
        let total = (nl + got).min(N - nx);
        if total > need { unsafe { self.push(s + n2, total - need); } }
        true
    }

    pub unsafe fn shrink(&mut self, s: usize, n: usize, n2: usize) {
        if n2 < n { unsafe { self.free(s + n2, n - n2); } }
    }
}
