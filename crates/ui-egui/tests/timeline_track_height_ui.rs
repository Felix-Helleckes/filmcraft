//! Headless UI tests of changing track heights by dragging the lines between track headers,
//! including the outer edges: the top of the highest video track and the bottom of the last audio
//! track.

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
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 1400.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        // short enough that every track is in view, with empty space above and below them
        d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 24.0, "audioTrackHeight": 24.0}}));
        d.frames(3);
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

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    fn heights(&self) -> (f32, f32) {
        let v = &self.app().ui.timeline;
        (v.video_track_h, v.audio_track_h)
    }

    /// Drag from the middle of the element `id` by `dy` points.
    fn drag(&mut self, id: &str, fy: f32, dy: f32) {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
        let (x, y) = (r[0] + r[2] / 2.0, r[1] + r[3] * fy);
        self.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x, "y": y + dy}}));
        self.frames(3);
    }

    fn labels(&self) -> (String, String) {
        let seq = self.app().session.active_sequence().unwrap();
        (format!("V{}", seq.video_tracks.len()), format!("A{}", seq.audio_tracks.len()))
    }
}

#[test]
fn the_top_edge_of_the_highest_video_track_resizes_the_video_tracks() {
    let mut d = Driver::demo();
    let (top, _) = d.labels();
    let (v0, a0) = d.heights();
    // dragging the outer edge up makes the video tracks taller, as the line below it does
    d.drag(&format!("timeline.track.{top}.resizeTop"), 0.5, -15.0);
    let (v1, a1) = d.heights();
    assert!(v1 > v0 + 5.0, "video tracks grow: {v0} -> {v1}");
    assert_eq!(a1, a0, "audio tracks are left alone");
    // and down makes them shorter again
    d.drag(&format!("timeline.track.{top}.resizeTop"), 0.5, 15.0);
    assert!(d.heights().0 < v1 - 5.0);
    // the line below it still works the same way
    let v2 = d.heights().0;
    d.drag(&format!("timeline.track.{top}.resize"), 0.5, -15.0);
    assert!(d.heights().0 > v2 + 5.0);
}

#[test]
fn the_bottom_edge_of_the_last_audio_track_resizes_the_audio_tracks_from_either_side_of_the_line() {
    let mut d = Driver::demo();
    let (_, last) = d.labels();
    let id = format!("timeline.track.{last}.resize");
    let (v0, a0) = d.heights();
    // from just past the line, in the empty space below the last track
    d.drag(&id, 0.9, 15.0);
    let (v1, a1) = d.heights();
    assert!(a1 > a0 + 5.0, "audio tracks grow: {a0} -> {a1}");
    assert_eq!(v1, v0, "video tracks are left alone");
    // and from just inside it
    d.drag(&id, 0.1, -15.0);
    assert!(d.heights().1 < a1 - 5.0);
}

/// #2046: over the track headers, Shift + wheel resizes every track and Option + wheel the tracks of
/// the kind under the pointer; over the clips the same wheel still scrolls or zooms the Timeline.
#[test]
fn wheel_over_track_headers_changes_track_heights() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 60.0, "audioTrackHeight": 60.0}}));
    d.frames(3);
    let wheel = |d: &mut Driver, id: &str, dy: f64, m: Value| {
        d.ok("ui.scroll", json!({"id": id, "dy": dy, "modifiers": m}));
        d.frames(2);
    };
    // Option over V1's header: video tracks only
    wheel(&mut d, "timeline.track.V1.target", 40.0, json!({"alt": true}));
    let (v, a) = d.heights();
    assert!(v > 60.0 && a == 60.0, "{v} {a}");
    // Shift over A1's header: every track
    wheel(&mut d, "timeline.track.A1.target", -40.0, json!({"shift": true}));
    let (v2, a2) = d.heights();
    assert!(v2 < v && a2 < 60.0, "{v2} {a2}");
    // over the clips Option + wheel zooms time and Shift + wheel scrolls sideways, as before
    let pps = d.app().ui.timeline.target_pps;
    wheel(&mut d, "timeline.tracks", 40.0, json!({"alt": true}));
    assert_eq!(d.heights(), (v2, a2));
    assert!(d.app().ui.timeline.target_pps != pps);
    d.ok("ui.set", json!({"timeline": {"scroll": 2.0}}));
    d.frames(2);
    let scroll = d.app().ui.timeline.target_scroll;
    wheel(&mut d, "timeline.tracks", 40.0, json!({"shift": true}));
    assert_eq!(d.heights(), (v2, a2));
    assert!(d.app().ui.timeline.target_scroll != scroll, "Shift + wheel over the clips scrolls sideways");
    // Cmd + wheel over a header scrolls that kind's tracks up and down, not sideways (tall tracks,
    // so there is something to scroll)
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 220.0, "audioTrackHeight": 220.0}}));
    d.frames(3);
    let (scroll, v_scroll, a_scroll) = (d.app().ui.timeline.target_scroll, d.app().ui.timeline.v_scroll, d.app().ui.timeline.a_scroll);
    wheel(&mut d, "timeline.track.A1.target", -40.0, json!({"command": true}));
    assert_eq!(d.app().ui.timeline.target_scroll, scroll, "not sideways");
    assert_eq!(d.app().ui.timeline.v_scroll, v_scroll, "the video tracks stay");
    assert!(d.app().ui.timeline.a_scroll != a_scroll, "the audio tracks scroll");
}
