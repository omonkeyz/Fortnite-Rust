//! `UAnimMontage` and `UAnimComposite`: names, slot tracks, sections and segments (tagged properties only; no
//! evaluation). Layout (UE 4.16):
//!
//! * `AnimMontage`: `BlendIn` / `BlendOut` (`FAlphaBlend`: `BlendTime`, `BlendOption`), `BlendOutTriggerTime`,
//!   `SyncGroup`, `SlotAnimTracks[]` (`SlotName`, `AnimTrack.AnimSegments[]`), `CompositeSections[]`
//!   (`SectionName`, `NextSectionName`, `StartTime`/`LinkValue`), `Notifies[]`, `SequenceLength`, `RateScale`,
//!   `BranchingPointMarkers[]`.
//! * `AnimSegment` (`FAnimSegment`): `AnimReference`, `StartPos` (position in the track), `AnimStartTime`,
//!   `AnimEndTime`, `AnimPlayRate` (1), `LoopingCount` (1).
//! * `AnimComposite`: `AnimationTrack.AnimSegments[]`, `SequenceLength`, `RateScale`.

use crate::sequence::{parse_notifies, Notify};
use crate::{Error, Result};
use fn_ueio::props::{read_props, Ctx, PropList};
use fn_ueio::{Package, Reader};

/// `FAnimSegment`.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimSegment {
    /// Object name of the referenced sequence.
    pub animation: Option<String>,
    pub animation_path: Option<String>,
    pub start_pos: f32,
    pub anim_start_time: f32,
    pub anim_end_time: f32,
    pub play_rate: f32,
    pub looping_count: i32,
}

impl AnimSegment {
    /// Duration this segment occupies in the track (`(end - start) / rate * loops`).
    pub fn track_length(&self) -> f32 {
        if self.play_rate == 0.0 {
            0.0
        } else {
            (self.anim_end_time - self.anim_start_time) / self.play_rate * self.looping_count.max(1) as f32
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlotTrack {
    pub slot_name: String,
    pub segments: Vec<AnimSegment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub name: String,
    pub next_section: String,
    pub start_time: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlphaBlend {
    pub time: f32,
    pub option: u8,
}

#[derive(Debug, Clone)]
pub struct AnimMontage {
    pub name: String,
    pub length: f32,
    pub rate_scale: f32,
    pub blend_in: AlphaBlend,
    pub blend_out: AlphaBlend,
    pub blend_out_trigger_time: f32,
    pub sync_group: Option<String>,
    pub slots: Vec<SlotTrack>,
    pub sections: Vec<Section>,
    pub notifies: Vec<Notify>,
    pub skeleton: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AnimComposite {
    pub name: String,
    pub length: f32,
    pub rate_scale: f32,
    pub segments: Vec<AnimSegment>,
    pub notifies: Vec<Notify>,
}

fn segments(pkg: &Package, track: &PropList) -> Vec<AnimSegment> {
    let mut out = Vec::new();
    if let Some(items) = track.array("AnimSegments") {
        for it in items {
            let Some(f) = it.as_fields() else { continue };
            let obj = f.object("AnimReference").unwrap_or(0);
            out.push(AnimSegment {
                animation: if obj == 0 { None } else { Some(pkg.index_name(obj)) },
                animation_path: if obj < 0 { Some(pkg.import_path(obj)) } else { None },
                start_pos: f.float("StartPos").unwrap_or(0.0) as f32,
                anim_start_time: f.float("AnimStartTime").unwrap_or(0.0) as f32,
                anim_end_time: f.float("AnimEndTime").unwrap_or(0.0) as f32,
                play_rate: f.float("AnimPlayRate").map(|x| x as f32).unwrap_or(1.0),
                looping_count: f.int("LoopingCount").map(|x| x as i32).unwrap_or(1),
            });
        }
    }
    out
}

fn blend(f: Option<&PropList>) -> AlphaBlend {
    let Some(f) = f else { return AlphaBlend { time: 0.25, option: 0 } };
    AlphaBlend {
        time: f.float("BlendTime").map(|x| x as f32).unwrap_or(0.25),
        option: match f.text("BlendOption").map(|s| s.rsplit("::").next().unwrap_or(s)) {
            Some("Linear") => 0,
            Some("Cubic") => 1,
            Some("HermiteCubic") => 2,
            Some("Sinusoidal") => 3,
            Some("QuadraticInOut") => 4,
            Some("CubicInOut") => 5,
            Some("QuarticInOut") => 6,
            Some("QuinticInOut") => 7,
            Some("CircularIn") => 8,
            Some("CircularOut") => 9,
            Some("CircularInOut") => 10,
            Some("ExpIn") => 11,
            Some("ExpOut") => 12,
            Some("ExpInOut") => 13,
            _ => 0,
        },
    }
}

fn export_props(pkg: &Package, class: &str) -> Result<(usize, PropList)> {
    let idx = pkg.find_export(class).ok_or_else(|| Error::Format(format!("package has no {class} export")))?;
    let (start, end) = pkg.export_range(idx);
    let mut r = Reader::at(&pkg.data[..end], start);
    let ctx = Ctx { names: &pkg.names, file_version: pkg.summary.file_version };
    Ok((idx, read_props(&mut r, &ctx)?))
}

impl AnimMontage {
    pub fn parse(pkg: &Package) -> Result<AnimMontage> {
        let (idx, props) = export_props(pkg, "AnimMontage")?;
        let mut slots = Vec::new();
        if let Some(items) = props.array("SlotAnimTracks") {
            for it in items {
                let Some(f) = it.as_fields() else { continue };
                slots.push(SlotTrack { slot_name: f.text("SlotName").unwrap_or("").to_string(), segments: f.fields("AnimTrack").map(|t| segments(pkg, t)).unwrap_or_default() });
            }
        }
        let mut sections = Vec::new();
        if let Some(items) = props.array("CompositeSections") {
            for it in items {
                let Some(f) = it.as_fields() else { continue };
                sections.push(Section {
                    name: f.text("SectionName").unwrap_or("").to_string(),
                    next_section: f.text("NextSectionName").unwrap_or("").to_string(),
                    // Relative link method (every pickaxe section): start = SegmentBeginTime + LinkValue (LinkValue alone is the offset, 0 for all of them)
                    start_time: f.float("StartTime").unwrap_or_else(|| { let (b, v) = (f.float("SegmentBeginTime").unwrap_or(0.0), f.float("LinkValue").unwrap_or(0.0)); match f.text("LinkMethod").map(|m| m.rsplit(':').next().unwrap_or(m)) { Some("Absolute") => v, Some("Proportional") => b + v * f.float("SegmentLength").unwrap_or(0.0), _ => b + v } }) as f32,
                });
            }
        }
        Ok(AnimMontage {
            name: pkg.exports[idx].object_name.clone(),
            length: props.float("SequenceLength").unwrap_or(0.0) as f32,
            rate_scale: props.float("RateScale").map(|x| x as f32).unwrap_or(1.0),
            blend_in: blend(props.fields("BlendIn")),
            blend_out: blend(props.fields("BlendOut")),
            blend_out_trigger_time: props.float("BlendOutTriggerTime").map(|x| x as f32).unwrap_or(-1.0),
            sync_group: props.text("SyncGroup").map(|s| s.to_string()).filter(|s| s != "None"),
            slots,
            sections,
            notifies: parse_notifies(pkg, &props),
            skeleton: props.object("Skeleton").filter(|&i| i != 0).map(|i| pkg.index_name(i)),
        })
    }

    /// Total length of all segments of the longest slot track.
    pub fn track_length(&self) -> f32 {
        self.slots.iter().map(|s| s.segments.iter().map(|g| g.start_pos + g.track_length()).fold(0.0, f32::max)).fold(0.0, f32::max)
    }
}

impl AnimComposite {
    pub fn parse(pkg: &Package) -> Result<AnimComposite> {
        let (idx, props) = export_props(pkg, "AnimComposite")?;
        let segs = props.fields("AnimationTrack").map(|t| segments(pkg, t)).unwrap_or_default();
        Ok(AnimComposite {
            name: pkg.exports[idx].object_name.clone(),
            length: props.float("SequenceLength").unwrap_or(0.0) as f32,
            rate_scale: props.float("RateScale").map(|x| x as f32).unwrap_or(1.0),
            segments: segs,
            notifies: parse_notifies(pkg, &props),
        })
    }
}
