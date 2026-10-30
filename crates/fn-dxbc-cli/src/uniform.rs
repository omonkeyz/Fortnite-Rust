//! `FMaterialUniformExpressionSet` of a cooked UE 4.16 material: parser and evaluator (port of tools/menu-material-eval.cjs,
//! tools/hud-uniforms.cjs, tools/sky-uniforms.cjs / settings-ui-uniforms.cjs for the 1.8 layout).
//!
//! The set is serialised inside the material's shader map (cooked `.uexp`); it is the recipe of the material constant buffer the compiled
//! shader reads: vector expressions fill `cb[i]` (one float4 each), scalar expressions are packed four per float4 after them.
//!
//! 1.8 serialisation (recovered from the bytes; byte-packed, NOT 4-aligned):
//! ```text
//!   list        = i32 count, then `count` expressions;   set = vectors, scalars, 2D textures
//!   expression  = FName type (8 bytes: name index, number) + payload
//!   VectorParameter = FName name, f32[4] default          ScalarParameter = FName name, f32 default
//!   Constant        = f32[4], u8 valueType                ComponentSwizzle = expr, i8[4] indices (-1 unused), u8 count
//!   FoldedMath      = A, B, u8 op (0 add, 1 sub, 2 mul, 3 div, 4 dot), u32 valueType
//!   AppendVector    = A, B, u32 numComponentsA            TrigMath = X, Y, u8 op (0 sin, 1 cos, 2 tan)
//!   Texture         = i32 texture index, i32 sampler source        TextureParameter = FName name + the same
//!   Time / RealTime = no payload
//! ```

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    VectorParameter { name: String, default: [f32; 4] },
    ScalarParameter { name: String, default: f32 },
    Constant { value: [f32; 4], value_type: u8 },
    ComponentSwizzle { e: Box<Expr>, idx: [i8; 4], count: u8 },
    FoldedMath { a: Box<Expr>, b: Box<Expr>, op: u8, value_type: u32 },
    AppendVector { a: Box<Expr>, b: Box<Expr>, num_a: u32 },
    TrigMath { x: Box<Expr>, y: Box<Expr>, op: u8 },
    /// `FMaterialUniformExpressionMax`: `Serialize` = A, B; evaluates the component-wise `max`.
    Max { a: Box<Expr>, b: Box<Expr> },
    Texture { index: i32, sampler_source: i32 },
    TextureParameter { name: String, index: i32, sampler_source: i32 },
    Time,
    RealTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UniformSet { pub vectors: Vec<Expr>, pub scalars: Vec<Expr>, pub textures2d: Vec<Expr> }

/// Byte-pattern candidates, not proven shader-map ownership. `end` includes
/// the trailing count but not that following list's unknown payload.
#[derive(Debug,Clone,PartialEq)]
pub struct SetCandidate{pub start:usize,pub end:usize,pub trailing_count:i32,pub set:UniformSet}
/// Exact observed ASCII FString immediately before a candidate. This is an
/// anchor only, not proof of the containing shader-map record's field layout.
pub fn preceding_label(uexp:&[u8],start:usize,label:&str)->bool{
    let Some(size)=label.len().checked_add(1) else{return false;};
    let Some(begin)=start.checked_sub(size).and_then(|n|n.checked_sub(4)) else{return false;};
    let Some(bytes)=uexp.get(begin..start) else{return false;};
    let Ok(count)=u32::try_from(size) else{return false;};
    bytes[..4]==count.to_le_bytes() && bytes[4..bytes.len()-1]==*label.as_bytes() && bytes.last()==Some(&0)
}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct CompilationDescription{pub start:usize,pub end:usize,pub text:String,pub preceding_bytes:Vec<u8>}
/// Observed 1.8 parent-material tail, not a general Unreal shader-map parser.
/// Preserve the 56 unknown preceding bytes; don't assign field semantics.
pub fn compilation_description(data:&[u8],candidate:&SetCandidate,label:&str)->Result<CompilationDescription,String>{
    if !preceding_label(data,candidate.start,label){return Err("candidate lacks original material-label anchor".into());}
    // 56 unknown bytes follow the set; a material with cube texture expressions (`trailing_count`) carries 16 more per cube expression
    // (OBSERVED in DownScopePostProcess: [i32 1][16 bytes] after the cube expression record; their meaning is not decoded)
    // OBSERVED in SkinAndMeshOutlinePostProcess (two material parameter collections, no cube): the record after the set starts with an i32 n
    // and the description sits 56 + 16 n bytes after it (one 16-byte id per collection; DownScopePostProcess has n = 1 and one cube)
    let n=data.get(candidate.end..candidate.end+4).map(|b|i32::from_le_bytes(b.try_into().unwrap())).filter(|n|(0..=16).contains(n)).unwrap_or(0) as usize;
    let mut last="description not found".to_owned();
    for gap in [56usize,56+16*candidate.trailing_count.max(0) as usize,56+16*n]{
        match description_at(data,candidate,label,gap){Ok(d)=>return Ok(d),Err(e)=>last=e}
    }
    Err(last)
}
fn description_at(data:&[u8],candidate:&SetCandidate,label:&str,gap:usize)->Result<CompilationDescription,String>{
    let start=candidate.end.checked_add(gap).ok_or("description offset overflow")?;
    let count_end=start.checked_add(4).ok_or("description offset overflow")?;
    let count=data.get(start..count_end).ok_or("truncated description count")?;
    let count=i32::from_le_bytes(count.try_into().unwrap());
    if !(1..=4096).contains(&count){return Err("unsupported description string count".into());}
    let end=count_end.checked_add(count as usize).ok_or("description range overflow")?;
    let bytes=data.get(count_end..end).ok_or("truncated description string")?;
    if bytes.last()!=Some(&0){return Err("unterminated description string".into());}
    let text=std::str::from_utf8(&bytes[..bytes.len()-1]).map_err(|e|e.to_string())?.to_owned();
    if !text.starts_with(&format!("Compiling {label}: Platform=")){return Err("description does not match original material label".into());}
    Ok(CompilationDescription{start,end,text,preceding_bytes:data[candidate.end..start].to_vec()})
}
pub fn set_candidates(uexp:&[u8],names:&[String])->Vec<SetCandidate>{
    let mut out=vec![];
    for h in 4..uexp.len().saturating_sub(7){
        let idx=i32::from_le_bytes(uexp[h..h+4].try_into().unwrap());
        let num=i32::from_le_bytes(uexp[h+4..h+8].try_into().unwrap());
        if idx<0 || !names.get(idx as usize).is_some_and(|n|n.starts_with("FMaterialUniformExpression")) || num!=0{continue;}
        let cnt=i32::from_le_bytes(uexp[h-4..h].try_into().unwrap());
        if !(1..=256).contains(&cnt){continue;}
        let mut r=R{d:uexp,o:h-4,names,number_suffix:true};
        let attempt=(||->PR<SetCandidate>{
            let vectors=r.list()?;let scalars=r.list()?;let textures2d=r.list()?;let trailing_count=r.i32()?;
            if vectors.is_empty() || !(0..64).contains(&trailing_count){return Err("trailing count".into());}
            // the cube texture expressions (`UniformCubeTextureExpressions`, `trailing_count` of them, same expression records as the 2D list) follow the count
            for _ in 0..trailing_count{r.expr(0)?;}
            Ok(SetCandidate{start:h-4,end:r.o,trailing_count,set:UniformSet{vectors,scalars,textures2d}})
        })();
        if let Ok(candidate)=attempt{out.push(candidate);}
    }
    out
}

#[cfg(test)]
mod candidate_tests{
    use super::*;
    #[test]fn description_requires_anchor_matching_label_and_complete_string(){
        let mut data=b"\x04\0\0\0Gun\0".to_vec();let c=SetCandidate{start:8,end:8,trailing_count:0,set:UniformSet{vectors:vec![],scalars:vec![],textures2d:vec![]}};
        data.extend([17u8;56]);let text=b"Compiling Gun: Platform=PCD3D_SM5";data.extend(((text.len()+1) as i32).to_le_bytes());data.extend(text);data.push(0);
        let desc=compilation_description(&data,&c,"Gun").unwrap();assert_eq!(desc.start,64);assert_eq!(desc.end,data.len());assert_eq!(desc.preceding_bytes,[17;56]);
        assert!(compilation_description(&data,&c,"Other").is_err());for n in 0..data.len(){assert!(compilation_description(&data[..n],&c,"Gun").is_err());}*data.last_mut().unwrap()=1;assert!(compilation_description(&data,&c,"Gun").is_err());
    }
    #[test]fn preceding_label_requires_exact_length_and_terminator(){let b=b"\x04\0\0\0Gun\0";assert!(preceding_label(b,b.len(),"Gun"));assert!(!preceding_label(b,b.len(),"Other"));for n in 0..b.len(){assert!(!preceding_label(b,n,"Gun"));}assert!(!preceding_label(b"\x04\0\0\0GunX",8,"Gun"));assert!(!preceding_label(b"\x05\0\0\0Gun\0",8,"Gun"));}
    #[test]fn reports_multiple_candidates_without_selecting_one(){
        let names=vec!["FMaterialUniformExpressionConstant".into()];let mut block=1i32.to_le_bytes().to_vec();block.extend([0u8;8]);for f in [1f32,2.,3.,4.]{block.extend(f.to_le_bytes());}block.push(3);block.extend([0u8;12]);
        let mut bytes=vec![255u8;7];bytes.extend(&block);bytes.extend([255u8;5]);bytes.extend(&block);
        let c=set_candidates(&bytes,&names);assert_eq!(c.len(),2);assert_eq!(c[0].start,7);assert_eq!(c[0].end,7+block.len());assert_eq!(c[1].start,12+block.len());assert_eq!(c[0].set,c[1].set);assert_eq!(parse_set(&bytes,&names).unwrap(),c[0].set);
        assert!(set_candidates(&[],&names).is_empty());assert!(set_candidates(&block[..block.len()-1],&names).is_empty());
    }
}

/// Parameter values of one material instance (its effective parameters through the parent chain).
#[derive(Debug, Clone, Default)]
pub struct Overrides { pub vectors: HashMap<String, [f32; 4]>, pub scalars: HashMap<String, f32> }

struct R<'a> { d: &'a [u8], o: usize, names: &'a [String], number_suffix: bool }
type PR<T> = Result<T, String>;
impl R<'_> {
    fn take(&mut self, n: usize) -> PR<&[u8]> {
        let s = self.d.get(self.o..self.o + n).ok_or_else(|| format!("read past end at {}", self.o))?;
        self.o += n;
        Ok(s)
    }
    fn i32(&mut self) -> PR<i32> { Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap())) }
    fn f32(&mut self) -> PR<f32> { Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap())) }
    fn u8(&mut self) -> PR<u8> { Ok(self.take(1)?[0]) }
    fn i8(&mut self) -> PR<i8> { Ok(self.take(1)?[0] as i8) }
    fn fname(&mut self) -> PR<String> {
        let i = self.i32()?;
        let n = self.i32()?;
        // FName instance numbers: `RGB_4` is (RGB, 5); the shop's rarity materials (`RGB_1..4`, `s_*`) need numbers above 4 (was 0..=4, which made the set of
        // M_Border_BattleLootTile_ColorBG fail to parse and fall back to a later, smaller candidate)
        if i < 0 || i as usize >= self.names.len() || !(0..=32).contains(&n) { return Err(format!("bad FName at {}", self.o - 8)); }
        let base = &self.names[i as usize];
        Ok(if n > 0 && self.number_suffix { format!("{base}_{}", n - 1) } else { base.clone() })
    }
    fn vec4(&mut self) -> PR<[f32; 4]> { Ok([self.f32()?, self.f32()?, self.f32()?, self.f32()?]) }
    fn expr(&mut self, depth: u32) -> PR<Expr> {
        if depth > 32 { return Err("expression nesting".into()); }
        let t = self.fname()?;
        let t = t.strip_prefix("FMaterialUniformExpression").unwrap_or(&t).to_string();
        Ok(match t.as_str() {
            "VectorParameter" => { let name = self.fname()?; Expr::VectorParameter { name, default: self.vec4()? } }
            "ScalarParameter" => { let name = self.fname()?; Expr::ScalarParameter { name, default: self.f32()? } }
            "Constant" => { let value = self.vec4()?; Expr::Constant { value, value_type: self.u8()? } }
            "ComponentSwizzle" => { let e = Box::new(self.expr(depth + 1)?); let idx = [self.i8()?, self.i8()?, self.i8()?, self.i8()?]; Expr::ComponentSwizzle { e, idx, count: self.u8()? } }
            "FoldedMath" => { let a = Box::new(self.expr(depth + 1)?); let b = Box::new(self.expr(depth + 1)?); let op = self.u8()?; Expr::FoldedMath { a, b, op, value_type: self.i32()? as u32 } }
            "AppendVector" => { let a = Box::new(self.expr(depth + 1)?); let b = Box::new(self.expr(depth + 1)?); Expr::AppendVector { a, b, num_a: self.i32()? as u32 } }
            "TrigMath" => { let x = Box::new(self.expr(depth + 1)?); let y = Box::new(self.expr(depth + 1)?); Expr::TrigMath { x, y, op: self.u8()? } }
            "Max" => { let a = Box::new(self.expr(depth + 1)?); let b = Box::new(self.expr(depth + 1)?); Expr::Max { a, b } }
            "Texture" => { let index = self.i32()?; Expr::Texture { index, sampler_source: self.i32()? } }
            "TextureParameter" => { let name = self.fname()?; let index = self.i32()?; Expr::TextureParameter { name, index, sampler_source: self.i32()? } }
            "Time" => Expr::Time,
            "RealTime" => Expr::RealTime,
            other => return Err(format!("unhandled uniform expression {other} at {}", self.o)),
        })
    }
    fn list(&mut self) -> PR<Vec<Expr>> {
        let n = self.i32()?;
        if !(0..=256).contains(&n) { return Err(format!("bad count {n}")); }
        (0..n).map(|_| self.expr(0)).collect()
    }
}

/// Finds and parses the uniform expression set in a cooked material's `.uexp` (`names` = the package name table).
/// Parameter names carry the FName instance number as `_<n-1>` (UE spelling: two parameters called `Size` become `Size` and `Size_1`).
pub fn parse_set(uexp: &[u8], names: &[String]) -> Result<UniformSet, String> { parse_set_with(uexp, names, true) }

/// Like [`parse_set`]; `number_suffix = false` reproduces tools/hud-uniforms.cjs, which dropped the FName number and therefore
/// conflated parameters that share a base name (`HeroHighlight-Radius` twice in the item card).
pub fn parse_set_with(uexp: &[u8], names: &[String], number_suffix: bool) -> Result<UniformSet, String> {
    let is_type = |i: i32| i >= 0 && (i as usize) < names.len() && names[i as usize].starts_with("FMaterialUniformExpression");
    let mut last_err = String::from("no candidate");
    let mut h = 4usize;
    while h + 8 <= uexp.len() {
        let idx = i32::from_le_bytes(uexp[h..h + 4].try_into().unwrap());
        let num = i32::from_le_bytes(uexp[h + 4..h + 8].try_into().unwrap());
        if is_type(idx) && num == 0 {
            let cnt = i32::from_le_bytes(uexp[h - 4..h].try_into().unwrap());
            if (1..=256).contains(&cnt) {
                let mut r = R { d: uexp, o: h - 4, names, number_suffix };
                let attempt = (|| -> PR<UniformSet> {
                    let vectors = r.list()?;
                    let scalars = r.list()?;
                    let textures2d = r.list()?;
                    let tc = r.i32()?;
                    if vectors.is_empty() || !(0..64).contains(&tc) { return Err("trailing count".into()); }
                    Ok(UniformSet { vectors, scalars, textures2d })
                })();
                match attempt { Ok(s) => return Ok(s), Err(e) => last_err = e }
            }
        }
        h += 1;
    }
    Err(format!("no uniform expression set found ({last_err})"))
}

/// Package name table of a cooked package.
pub fn package_names(uasset: &[u8], uexp: &[u8]) -> Result<Vec<String>, String> {
    let p = fn_uasset::package::Package::parse(uasset, uexp).map_err(|e| e.to_string())?;
    Ok(p.names.clone())
}

pub fn eval_vec(e: &Expr, ov: &Overrides, time: f32) -> [f32; 4] {
    match e {
        Expr::VectorParameter { name, default } => ov.vectors.get(name).copied().unwrap_or(*default),
        Expr::ScalarParameter { name, default } => { let s = ov.scalars.get(name).copied().unwrap_or(*default); [s; 4] }
        Expr::Constant { value, .. } => *value,
        Expr::ComponentSwizzle { e, idx, .. } => { let v = eval_vec(e, ov, time); std::array::from_fn(|k| if idx[k] >= 0 { v[idx[k] as usize & 3] } else { 0.0 }) }
        Expr::AppendVector { a, b, num_a } => {
            let (a, b) = (eval_vec(a, ov, time), eval_vec(b, ov, time));
            std::array::from_fn(|k| if (k as u32) < *num_a { a[k] } else { b.get(k - *num_a as usize).copied().unwrap_or(0.0) })
        }
        Expr::FoldedMath { a, b, op, .. } => {
            let (a, b) = (eval_vec(a, ov, time), eval_vec(b, ov, time));
            if *op == 4 { let s = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]; return [s; 4]; }
            std::array::from_fn(|k| match op { 0 => a[k] + b[k], 1 => a[k] - b[k], 2 => a[k] * b[k], _ => if b[k] == 0.0 { 0.0 } else { a[k] / b[k] } })
        }
        Expr::Max { a, b } => { let (a, b) = (eval_vec(a, ov, time), eval_vec(b, ov, time)); std::array::from_fn(|k| a[k].max(b[k])) }
        Expr::TrigMath { x, op, .. } => { let v = eval_vec(x, ov, time); v.map(|x| match op { 0 => x.sin(), 1 => x.cos(), _ => x.tan() }) }
        Expr::Time | Expr::RealTime => [time; 4],
        Expr::Texture { .. } | Expr::TextureParameter { .. } => [0.0; 4],
    }
}

pub fn eval_vec_by(e: &Expr, v: &dyn Fn(&str) -> Option<[f32; 4]>, sc: &dyn Fn(&str) -> Option<f32>, time: f32) -> [f32; 4] {
    match e {
        Expr::VectorParameter { name, default } => v(name).unwrap_or(*default),
        Expr::ScalarParameter { name, default } => { let s = sc(name).unwrap_or(*default); [s; 4] }
        Expr::Constant { value, .. } => *value,
        Expr::ComponentSwizzle { e, idx, .. } => { let x = eval_vec_by(e, v, sc, time); std::array::from_fn(|k| if idx[k] >= 0 { x[idx[k] as usize & 3] } else { 0.0 }) }
        Expr::AppendVector { a, b, num_a } => {
            let (a, b) = (eval_vec_by(a, v, sc, time), eval_vec_by(b, v, sc, time));
            std::array::from_fn(|k| if (k as u32) < *num_a { a[k] } else { b.get(k - *num_a as usize).copied().unwrap_or(0.0) })
        }
        Expr::FoldedMath { a, b, op, .. } => {
            let (a, b) = (eval_vec_by(a, v, sc, time), eval_vec_by(b, v, sc, time));
            if *op == 4 { let s = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]; return [s; 4]; }
            std::array::from_fn(|k| match op { 0 => a[k] + b[k], 1 => a[k] - b[k], 2 => a[k] * b[k], _ => if b[k] == 0.0 { 0.0 } else { a[k] / b[k] } })
        }
        Expr::Max { a, b } => { let (a, b) = (eval_vec_by(a, v, sc, time), eval_vec_by(b, v, sc, time)); std::array::from_fn(|k| a[k].max(b[k])) }
        Expr::TrigMath { x, op, .. } => { let x = eval_vec_by(x, v, sc, time); x.map(|x| match op { 0 => x.sin(), 1 => x.cos(), _ => x.tan() }) }
        Expr::Time | Expr::RealTime => [time; 4],
        Expr::Texture { .. } | Expr::TextureParameter { .. } => [0.0; 4],
    }
}

pub fn evaluate_cb_by(set: &UniformSet, v: &dyn Fn(&str) -> Option<[f32; 4]>, sc: &dyn Fn(&str) -> Option<f32>, time: f32, out: &mut Vec<[f32; 4]>) {
    let nv = set.vectors.len();
    let regs = nv + set.scalars.len().div_ceil(4);
    out.clear();
    out.resize(regs, [0.0; 4]);
    for (i, e) in set.vectors.iter().enumerate() { out[i] = eval_vec_by(e, v, sc, time); }
    for (i, e) in set.scalars.iter().enumerate() { out[nv + (i >> 2)][i & 3] = eval_vec_by(e, v, sc, time)[0]; }
}

fn label(e: &Expr) -> String {
    match e {
        Expr::VectorParameter { name, .. } | Expr::ScalarParameter { name, .. } | Expr::TextureParameter { name, .. } => name.clone(),
        Expr::Constant { .. } => "Constant()".into(),
        Expr::ComponentSwizzle { e, .. } => format!("ComponentSwizzle({})", label(e)),
        Expr::FoldedMath { a, b, .. } => format!("FoldedMath({},{})", label(a), label(b)),
        Expr::AppendVector { a, b, .. } => format!("AppendVector({},{})", label(a), label(b)),
        Expr::TrigMath { x, y, .. } => format!("TrigMath({},{})", label(x), label(y)),
        Expr::Max { a, b } => format!("Max({},{})", label(a), label(b)),
        Expr::Texture { .. } => "Texture()".into(),
        Expr::Time => "Time()".into(),
        Expr::RealTime => "RealTime()".into(),
    }
}

/// The packed material constant buffer.
#[derive(Debug, Clone)]
pub struct Packed {
    pub registers: usize,
    pub vectors: usize,
    pub scalars: usize,
    pub cb: Vec<[f32; 4]>,
    /// `cb[i]` / `cb[i].x` -> expression label
    pub labels: Vec<(String, String)>,
}

pub fn evaluate(set: &UniformSet, ov: &Overrides, time: f32) -> Packed {
    let vec: Vec<[f32; 4]> = set.vectors.iter().map(|e| eval_vec(e, ov, time)).collect();
    let sc: Vec<f32> = set.scalars.iter().map(|e| eval_vec(e, ov, time)[0]).collect();
    let nv = vec.len();
    let regs = nv + sc.len().div_ceil(4);
    let mut cb = vec![[0.0f32; 4]; regs];
    cb[..nv].copy_from_slice(&vec);
    for (i, v) in sc.iter().enumerate() { cb[nv + (i >> 2)][i & 3] = *v; }
    let mut labels = vec![];
    for (i, e) in set.vectors.iter().enumerate() { labels.push((format!("cb[{i}]"), label(e))); }
    for (i, e) in set.scalars.iter().enumerate() { labels.push((format!("cb[{}].{}", nv + (i >> 2), ['x', 'y', 'z', 'w'][i & 3]), label(e))); }
    Packed { registers: regs, vectors: nv, scalars: sc.len(), cb, labels }
}

