use serde_json::json;

use crate::Session;
use filmcraft_project::{ItemId, ParamValue, TrackKind};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// Mean brightness of row `y` of a rendered frame (red + green).
fn row(f: &filmcraft_render::Image, y: usize) -> f32 {
    (0..f.w).map(|x| f.px[(y * f.w + x) * 4] + f.px[(y * f.w + x) * 4 + 1]).sum::<f32>() / f.w as f32
}

/// Auto Reframe Sequence (#1564): a 9:16 copy of the demo's 16:9 edit whose clips fill the tall
/// frame (the effect alone left them letterboxed), in one undo step, the original untouched.
#[test]
fn auto_reframe_sequence_fills_a_vertical_copy() {
    let mut s = demo();
    let source = s.state.active_sequence.unwrap();
    let before = s.active_sequence().unwrap().clone();
    assert_eq!((before.settings.width, before.settings.height), (1920, 1080));
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    let letterboxed = s.render_program(0.25).unwrap();
    let r = s.execute("sequence.autoReframe", json!({"aspect": "9:16", "motion": "faster"})).unwrap();
    let id = ItemId(r["sequence"].as_u64().unwrap());
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(1080), Some(1920)), "{r}");
    assert_ne!(id, source);
    assert_eq!(s.state.active_sequence, Some(id), "the new sequence opens");
    assert!(s.project.item(id).unwrap().name.ends_with("Vertical 9x16"), "{r}");
    assert_eq!(s.project.sequence(source).unwrap(), &before, "the original is untouched");

    // every video clip with a picture: Auto Reframe at 9:16, faster; scaled to cover the frame
    let q = s.active_sequence().unwrap().clone();
    let mut reframed = 0;
    for it in q.all_tracks().filter(|t| t.kind == TrackKind::Video).flat_map(|t| &t.items) {
        let Some(e) = it.effects.iter().find(|e| e.effect == "auto_reframe") else {
            assert!(it.graphic.is_some() || s.project.source_par(it.item).is_none(), "{} was not reframed", it.name);
            continue;
        };
        reframed += 1;
        assert_eq!(e.param("aspect").map(|p| p.value.clone()), Some(ParamValue::Choice(1)), "{}", it.name);
        assert_eq!(e.param("preset").map(|p| p.value.clone()), Some(ParamValue::Choice(2)), "{}", it.name);
        assert!(!it.scale_to_frame);
        let old = before.find_item(it.id).unwrap().1;
        let old_scale = old.effect("motion").map_or(100.0, |m| m.f64_at("scale", filmcraft_time::Tick::ZERO));
        let (w, h) = s.project.source_size(it.item).unwrap();
        if (w, h) == (1920, 1080) && !old.scale_to_frame {
            let scale = it.effect("motion").unwrap().f64_at("scale", filmcraft_time::Tick::ZERO);
            assert!((scale - old_scale * 1920.0 / 1080.0).abs() < 0.01, "{}: {old_scale} → {scale}", it.name);
        }
    }
    assert_eq!(r["clips"].as_u64(), Some(reframed), "{r}");
    assert!(reframed >= 4, "{r}");

    // the picture reaches the top of the tall frame: the 16:9 edit in a 9:16 frame would be black there
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    let tall = s.render_program(0.25).unwrap();
    assert_eq!((tall.w, tall.h), (270, 480));
    assert!(row(&tall, 2) > 0.005 && row(&tall, tall.h - 3) > 0.005, "top {} bottom {}", row(&tall, 2), row(&tall, tall.h - 3));
    assert!(row(&letterboxed, letterboxed.h / 2) > 0.05);

    // one undo step removes it
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(id).is_none());
    s.execute("edit.redo", json!({})).unwrap();
    assert!(s.project.sequence(id).is_some());
}

#[test]
fn auto_reframe_sizes_and_refusals() {
    let mut s = demo();
    let source = s.state.active_sequence.unwrap();
    for (aspect, size) in [("1:1", (1080, 1080)), ("4:5", (1080, 1350)), ("16:9", (1920, 1080))] {
        let r = s.execute("sequence.autoReframe", json!({"sequence": source.0, "aspect": aspect, "name": format!("Cut {aspect}")})).unwrap();
        let q = s.project.sequence(ItemId(r["sequence"].as_u64().unwrap())).unwrap();
        assert_eq!((q.settings.width, q.settings.height), size, "{aspect}");
        assert_eq!(s.project.item(ItemId(r["sequence"].as_u64().unwrap())).unwrap().name, format!("Cut {aspect}"));
        s.active_sequence().unwrap().check().unwrap();
    }
    for bad in [json!({"aspect": "2:1"}), json!({"aspect": "9:16", "motion": "sideways"}), json!({"sequence": 999_999, "aspect": "1:1"})] {
        assert!(s.execute("sequence.autoReframe", bad.clone()).is_err(), "{bad}");
    }
}
