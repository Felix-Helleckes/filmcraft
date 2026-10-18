//! Headless UI test of the caption track font picker in the Text panel's Captions tab: picking a
//! family sets the track style's font, and macOS-private (`.`-prefixed) faces are not offered.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn font(&mut self, track: u64) -> String {
        let r = self.ok("engine.execute", json!({"command": "captions.list", "params": {}}));
        let t = r["tracks"].as_array().unwrap().iter().find(|t| t["id"] == json!(track)).expect("track").clone();
        t["style"]["font"].as_str().unwrap().to_string()
    }
}

#[test]
fn caption_font_picker_sets_the_track_font() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    let track = d.ok("engine.execute", json!({"command": "captions.newTrack", "params": {"format": "Subtitle"}}))["track"].as_u64().unwrap();
    d.ok("engine.execute", json!({"command": "captions.add", "params": {"track": track, "text": "Hello", "seconds": 1.0}}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.tab.Captions"}));
    d.frames(3);
    assert_eq!(d.font(track), "Inter");

    d.ok("ui.click", json!({"id": "text.captions.style.font"}));
    d.frames(3);
    let options = d.ids("text.captions.style.font.option.");
    assert!(!options.is_empty(), "the font list is open");
    assert!(!options.iter().any(|o| o.starts_with("text.captions.style.font.option..")), "private faces offered: {options:?}");
    // with system fonts the list is long: scroll until the bundled serif is on screen
    let serif = "text.captions.style.font.option.Noto Serif";
    for _ in 0..200 {
        let shown = d.ids("text.captions.style.font.option.");
        if shown.iter().any(|o| o == serif) {
            break;
        }
        // wheel over a row in the middle (the edge rows are clipped by the popup)
        let Some(mid) = shown.get(shown.len() / 2) else { break };
        d.ok("ui.scroll", json!({"id": mid, "dy": -120.0}));
        d.frames(2);
    }
    d.ok("ui.click", json!({"id": serif}));
    d.frames(3);
    assert_eq!(d.font(track), "Noto Serif");
}

/// The caption style strip sets how far the captions sit from the frame edge (#1364): a drag on
/// the margin is one `captions.setStyle`, and the captions move in the picture.
#[test]
fn caption_margin_moves_the_captions() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    let track = d.ok("engine.execute", json!({"command": "captions.newTrack", "params": {"format": "Subtitle"}}))["track"].as_u64().unwrap();
    d.ok("engine.execute", json!({"command": "playhead.set", "params": {"seconds": 1.0}}));
    d.ok("engine.execute", json!({"command": "captions.add", "params": {"track": track, "text": "Hello", "durationSeconds": 2.0}}));
    d.ok("ui.click", json!({"id": "text.tab.Captions"}));
    d.frames(3);
    let margin = |d: &mut Driver| -> f64 {
        let r = d.ok("engine.execute", json!({"command": "captions.list", "params": {}}));
        r["tracks"].as_array().unwrap().iter().find(|t| t["id"] == json!(track)).unwrap()["style"]["margin"].as_f64().unwrap()
    };
    let before = margin(&mut d);
    assert!((before - 0.08).abs() < 1e-6, "{before}");
    // the lowest caption row in the picture, before and after
    let lowest = |d: &mut Driver| -> usize {
        let s = &mut d.harness.state_mut().session;
        let with = s.render_program(0.25).unwrap();
        s.execute("captions.hideAll", json!({})).unwrap();
        let without = s.render_program(0.25).unwrap();
        s.execute("captions.showAll", json!({})).unwrap();
        (0..with.h).rev().find(|y| (0..with.w * 4).any(|x| (with.px[y * with.w * 4 + x] - without.px[y * with.w * 4 + x]).abs() > 0.05)).unwrap()
    };
    let low0 = lowest(&mut d);
    // every control of the style strip is inside the panel (in one row they ran past its edge)
    let panel = d.ok("ui.elements", json!({"prefix": "panel.Text"}))[0]["rect"].clone();
    let right = panel[0].as_f64().unwrap() + panel[2].as_f64().unwrap();
    for e in d.ok("ui.elements", json!({"prefix": "text.captions.style."})).as_array().unwrap() {
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        assert!(r[0] + r[2] <= right + 0.5, "{} ends at {} past the panel's {right}", e["id"], r[0] + r[2]);
    }
    let v = d.ok("ui.elements", json!({"prefix": "text.captions.style.margin"}));
    let r: Vec<f64> = v[0]["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
    let (cx, cy) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": cx, "y": cy}, "to": {"x": cx + 60.0, "y": cy}, "steps": 6}));
    d.frames(3);
    let after = margin(&mut d);
    assert!(after > before + 0.05, "{before} -> {after}");
    let low1 = lowest(&mut d);
    assert!(low1 + 10 < low0, "bottom-anchored captions moved up: {low0} -> {low1}");
    // the size field next to it: a drag sets the size it ends on (it read back the old one)
    let size = |d: &mut Driver| -> f64 {
        let r = d.ok("engine.execute", json!({"command": "captions.list", "params": {}}));
        r["tracks"].as_array().unwrap().iter().find(|t| t["id"] == json!(track)).unwrap()["style"]["size"].as_f64().unwrap()
    };
    let s0 = size(&mut d);
    let v = d.ok("ui.elements", json!({"prefix": "text.captions.style.size"}));
    let r: Vec<f64> = v[0]["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
    let (cx, cy) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": cx, "y": cy}, "to": {"x": cx - 30.0, "y": cy}, "steps": 6}));
    d.frames(3);
    let s1 = size(&mut d);
    assert!(s1 < s0 - 5.0, "{s0} -> {s1}");
}
