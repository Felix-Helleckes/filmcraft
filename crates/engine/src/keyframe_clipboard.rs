//! Copy and paste of keyframes in Effect Controls (#2582): copied keys keep their effect,
//! parameter and spacing; Paste puts them on the same parameters of the clip shown, from the
//! playhead, as one undo step.
use std::collections::BTreeMap;

use filmcraft_project::{ClipId, Keyframe, Param, TrackItem, TrackKind};
use filmcraft_time::Tick;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{EffectSel, Result, Session, commands::bad};

const COPY: &str = "effects.copyKeyframes";
const PASTE: &str = "effects.pasteKeyframes";
const MAX_KEYS: usize = 4096;

/// One copied keyframe: where it came from (the effect by its id and which instance of that id,
/// so it finds the same effect on another clip) and its offset from the first copied key.
#[derive(Clone, Debug)]
pub struct CopiedKeyframe {
    kind: TrackKind,
    effect: EffectSel,
    mask: Option<usize>,
    param: String,
    offset: Tick,
    key: Keyframe,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    clip: u64,
    effect: usize,
    param: String,
    #[serde(default)]
    mask: Option<usize>,
    time: Tick,
}

fn param<'a>(it: &'a TrackItem, effect: usize, mask: Option<usize>, name: &str) -> Option<&'a Param> {
    let e = it.effects.get(effect)?;
    match mask {
        Some(m) => e.masks.get(m)?.param(name),
        None => e.params.get(name),
    }
}

/// Timeline time at which the clip shows media time `m` (as Effect Controls draws its keys).
fn timeline_time_of(it: &TrackItem, m: Tick) -> Tick {
    let offset = if it.reverse { it.source_out() - m - Tick(1) } else { m - it.source_in };
    it.start + Tick((offset.0 as f64 / it.speed.abs().max(1e-6)).round() as i64)
}

pub(crate) fn copy(s: &mut Session, p: &Value) -> Result<Value> {
    let raw = p.get("keyframes").and_then(Value::as_array).ok_or_else(|| bad(COPY, "need keyframes"))?;
    if raw.is_empty() || raw.len() > MAX_KEYS {
        return Err(bad(COPY, "keyframe count must be between 1 and 4096"));
    }
    let keys: Vec<Key> = serde_json::from_value(Value::Array(raw.clone())).map_err(|_| bad(COPY, "invalid keyframe reference"))?;
    let q = s.active_sequence().ok_or(crate::EngineError::NoSequence)?;
    let mut copied = Vec::with_capacity(keys.len());
    for k in &keys {
        let (tid, it) = q.find_item(ClipId(k.clip)).ok_or_else(|| bad(COPY, "clip does not exist"))?;
        let kind = q.track(tid).map_or(TrackKind::Video, |t| t.kind);
        let effect = EffectSel::of(ClipId(k.clip), &it.effects, k.effect).ok_or_else(|| bad(COPY, "effect does not exist"))?;
        let found = param(it, k.effect, k.mask, &k.param).and_then(|p| p.keyframes.iter().find(|kf| kf.time == k.time));
        let key = found.ok_or_else(|| bad(COPY, "keyframe does not exist"))?.clone();
        let at = timeline_time_of(it, k.time);
        copied.push(CopiedKeyframe { kind, effect, mask: k.mask, param: k.param.clone(), offset: at, key });
    }
    let first = copied.iter().map(|c| c.offset).min().unwrap_or(Tick::ZERO);
    for c in &mut copied {
        c.offset -= first;
    }
    let n = copied.len();
    s.state.keyframe_clipboard = copied;
    Ok(json!({"copied": n}))
}

/// The clips keyframes are pasted into: the `clips` named, else the selected ones; the first of
/// each track kind (Effect Controls shows one clip and the audio linked to it).
fn targets(s: &Session, p: &Value) -> Result<Vec<(ClipId, TrackKind)>> {
    let q = s.active_sequence().ok_or(crate::EngineError::NoSequence)?;
    let ids: Vec<ClipId> = match p.get("clips") {
        Some(v) => v
            .as_array()
            .ok_or_else(|| bad(PASTE, "clips must be a list of ids"))?
            .iter()
            .map(|c| c.as_u64().map(ClipId))
            .collect::<Option<_>>()
            .ok_or_else(|| bad(PASTE, "clips must be a list of ids"))?,
        None => s.state.selection.clone(),
    };
    let mut out: Vec<(ClipId, TrackKind)> = Vec::new();
    for id in ids {
        let (tid, _) = q.find_item(id).ok_or_else(|| bad(PASTE, "clip does not exist"))?;
        let kind = q.track(tid).map_or(TrackKind::Video, |t| t.kind);
        if !out.iter().any(|(_, k)| *k == kind) {
            out.push((id, kind));
        }
    }
    Ok(out)
}

pub(crate) fn paste(s: &mut Session, p: &Value) -> Result<Value> {
    if s.state.keyframe_clipboard.is_empty() {
        return Err(bad(PASTE, "no keyframes have been copied"));
    }
    let targets = targets(s, p)?;
    if targets.is_empty() {
        return Err(bad(PASTE, "select a clip to paste the keyframes into"));
    }
    let at = s.playhead();
    let q = s.active_sequence().ok_or(crate::EngineError::NoSequence)?;
    // (clip, effect index, mask, param) -> the keys to put there
    let mut lanes: BTreeMap<(u64, usize, Option<usize>, String), Vec<Keyframe>> = BTreeMap::new();
    let mut pasted = Vec::new();
    for c in &s.state.keyframe_clipboard {
        let Some(&(clip, _)) = targets.iter().find(|(_, k)| *k == c.kind) else { continue };
        let Some((_, it)) = q.find_item(clip) else { continue };
        let Some(effect) = c.effect.index(&it.effects) else { continue };
        if param(it, effect, c.mask, &c.param).is_none() {
            continue;
        }
        let t = at + c.offset;
        // only where the clip is: a key past its end would be out of reach in Effect Controls
        if t < it.start || t >= it.end() {
            continue;
        }
        let mut key = c.key.clone();
        key.time = it.source_time_at(t);
        let mut r = json!({"clip": clip.0, "effect": effect, "param": c.param, "time": key.time});
        if let Some(m) = c.mask {
            r["mask"] = json!(m);
        }
        pasted.push(r);
        lanes.entry((clip.0, effect, c.mask, c.param.clone())).or_default().push(key);
    }
    if lanes.is_empty() {
        return Err(bad(PASTE, "none of the copied keyframes fit: the clip needs the same effects and parameters, under the playhead"));
    }
    s.edit_sequence("Paste Keyframes", |q, _, _| {
        for ((clip, effect, mask, name), keys) in lanes {
            let Some((_, it)) = q.find_item_mut(ClipId(clip)) else { continue };
            let Some(e) = it.effects.get_mut(effect) else { continue };
            let param = match mask {
                Some(m) => e.masks.get_mut(m).and_then(|m| m.param_mut(&name)),
                None => e.params.get_mut(&name),
            };
            let Some(param) = param else { continue };
            // a pasted key replaces one at the same time
            param.keyframes.retain(|k| !keys.iter().any(|n| n.time == k.time));
            param.keyframes.extend(keys);
            param.keyframes.sort_by_key(|k| k.time);
        }
        Ok(())
    })?;
    Ok(json!({"pasted": pasted.len(), "keyframes": pasted}))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The demo's first video clip with Motion keyframes on Scale at 0.5 s and 1 s and Rotation at 1.5 s.
    fn fixture() -> (Session, ClipId, Vec<Value>) {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
        let mut keys = Vec::new();
        for (param, seconds) in [("scale", 0.5), ("scale", 1.0), ("rotation", 1.5)] {
            s.execute("playhead.set", json!({"seconds": seconds})).unwrap();
            s.execute("effects.addKeyframe", json!({"clip": clip.0, "effect": "motion", "param": param})).unwrap();
            let it = s.active_sequence().unwrap().find_item(clip).unwrap().1;
            let effect = it.effects.iter().position(|e| e.effect == "motion").unwrap();
            keys.push(json!({"clip": clip.0, "effect": effect, "param": param, "time": it.source_time_at(s.playhead()).0}));
        }
        (s, clip, keys)
    }

    fn times(s: &Session, clip: ClipId, param: &str) -> Vec<Tick> {
        let it = s.active_sequence().unwrap().find_item(clip).unwrap().1;
        let e = it.effects.iter().find(|e| e.effect == "motion").unwrap();
        e.params[param].keyframes.iter().map(|k| it.start + (k.time - it.source_in)).collect()
    }

    #[test]
    fn pasted_keyframes_keep_their_spacing_from_the_playhead_as_one_undo_step() {
        let (mut s, clip, keys) = fixture();
        let other = s.active_sequence().unwrap().video_tracks[0].items[1].id;
        s.execute(COPY, json!({"keyframes": keys})).unwrap();
        let start = s.active_sequence().unwrap().find_item(other).unwrap().1.start;
        let at = start + Tick::from_seconds_f64(0.25);
        s.execute("playhead.set", json!({"time": at.0})).unwrap();
        // the playhead snaps to a frame
        let at = s.playhead();
        s.execute("timeline.select", json!({"clips": [other.0]})).unwrap();
        let before = s.project.clone();
        let undo = s.history.undo.len();
        let r = s.execute(PASTE, json!({})).unwrap();
        assert_eq!(r["pasted"], 3, "{r}");
        let source = times(&s, clip, "scale");
        let spacing = source[1] - source[0];
        let scale = times(&s, other, "scale");
        assert_eq!(scale.len(), 2);
        assert!((scale[0] - at).0.abs() <= 1, "the first key lands on the playhead");
        assert!(((scale[1] - scale[0]) - spacing).0.abs() <= 1, "spacing kept");
        assert_eq!(times(&s, other, "rotation").len(), 1);
        // values and interpolation come along
        let value = |s: &Session, c: ClipId| {
            let it = s.active_sequence().unwrap().find_item(c).unwrap().1;
            serde_json::to_value(&it.effects.iter().find(|e| e.effect == "motion").unwrap().params["scale"].keyframes[0].value).unwrap()
        };
        assert_eq!(value(&s, other), value(&s, clip));
        assert_eq!(s.history.undo.len(), undo + 1);
        s.undo();
        assert_eq!(serde_json::to_value(&s.project).unwrap(), serde_json::to_value(&before).unwrap());
    }

    #[test]
    fn keys_past_the_clip_end_are_left_out_and_nothing_fitting_changes_nothing() {
        let (mut s, clip, keys) = fixture();
        s.execute(COPY, json!({"keyframes": keys})).unwrap();
        let end = s.active_sequence().unwrap().find_item(clip).unwrap().1.end();
        // 0.5 s before the end: the first Scale key fits, the one 0.5 s later and Rotation don't
        s.execute("playhead.set", json!({"time": (end - Tick::from_seconds_f64(0.5)).0})).unwrap();
        s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
        let r = s.execute(PASTE, json!({})).unwrap();
        assert_eq!(r["pasted"], 1, "{r}");
        assert_eq!(times(&s, clip, "scale").len(), 3);
        // past the clip: an error, and no edit
        s.execute("playhead.set", json!({"time": end.0 + Tick::from_seconds_f64(60.0).0})).unwrap();
        let before = s.project.clone();
        assert!(s.execute(PASTE, json!({"clips": [clip.0]})).is_err());
        assert!(std::sync::Arc::ptr_eq(&before, &s.project));
    }

    #[test]
    fn hostile_requests_are_refused() {
        let (mut s, clip, keys) = fixture();
        assert!(s.execute(PASTE, json!({})).is_err(), "nothing copied yet");
        let mut missing = keys[0].clone();
        missing["time"] = json!(-5);
        for request in [
            json!({}),
            json!({"keyframes": []}),
            json!({"keyframes": [missing]}),
            json!({"keyframes": vec![keys[0].clone(); MAX_KEYS + 1]}),
            json!({"keyframes": [{"clip": clip.0, "effect": u64::MAX, "param": "scale", "time": 0}]}),
            json!({"keyframes": [{"clip": u64::MAX, "effect": 0, "param": "scale", "time": 0}]}),
        ] {
            assert!(s.execute(COPY, request).is_err());
        }
        assert!(s.state.keyframe_clipboard.is_empty(), "a refused copy leaves the clipboard alone");
        s.execute(COPY, json!({"keyframes": keys})).unwrap();
        for request in [json!({"clips": "x"}), json!({"clips": [u64::MAX]}), json!({"clips": [-1]}), json!({"clips": []})] {
            assert!(s.execute(PASTE, request).is_err());
        }
    }
}
