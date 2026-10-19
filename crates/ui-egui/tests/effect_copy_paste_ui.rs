//! Headless UI tests of copying and pasting effects in Effect Controls (#1680).

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
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
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

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn clip(&mut self, i: usize) -> filmcraft_engine::project::TrackItem {
        self.app().session.active_sequence().unwrap().video_tracks[0].items[i].clone()
    }

    fn right_click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id, "button": "right"}));
        self.frames(3);
    }

    fn blurriness(&mut self, i: usize) -> Vec<f64> {
        self.clip(i).effects.iter().filter(|e| e.effect == "gaussian_blur").filter_map(|e| e.param("blurriness")?.value.as_f64()).collect()
    }
}

#[test]
fn effects_copy_and_paste_from_the_menu_and_the_keyboard() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Effects"}));
    let (a, b) = (d.clip(0).id.0, d.clip(2).id.0);
    d.exec("effects.apply", json!({"clips": [a], "effect": "gaussian_blur"}));
    d.exec("effects.setParam", json!({"clip": a, "effect": "gaussian_blur", "param": "blurriness", "value": 30.0}));
    d.exec("timeline.select", json!({"clips": [a]}));
    d.frames(3);
    // fold the fixed effects so the blur's row is in view
    for fx in ["motion", "opacity", "time_remap"] {
        d.click(&format!("effectControls.effect.{fx}.twirl"));
    }

    // the effect's right-click menu: Copy, then Paste on the other clip's Effect Controls
    d.right_click("effectControls.effect.gaussian_blur");
    d.click("effectControls.effect.gaussian_blur.copy");
    d.exec("timeline.select", json!({"clips": [b]}));
    d.frames(3);
    assert!(d.blurriness(2).is_empty());
    let undo = d.app().session.history.undo.len();
    d.right_click("effectControls.effect.motion");
    d.click("effectControls.effect.motion.paste");
    assert_eq!(d.blurriness(2), vec![30.0], "the blur is pasted with its setting");
    assert_eq!(d.app().session.history.undo.len(), undo + 1, "one undo step");

    // Paste Settings puts the copied setting back on an effect of the same kind
    d.exec("effects.setParam", json!({"clip": b, "effect": "gaussian_blur", "param": "blurriness", "value": 5.0}));
    d.frames(2);
    for fx in ["motion", "opacity", "time_remap"] {
        d.click(&format!("effectControls.effect.{fx}.twirl"));
    }
    d.right_click("effectControls.effect.gaussian_blur");
    d.click("effectControls.effect.gaussian_blur.pasteSettings");
    assert_eq!(d.blurriness(2), vec![30.0]);

    // Cmd+C / Cmd+V in Effect Controls copy the selected effect and paste it onto the shown clip,
    // not a clip onto the Timeline
    let clips = d.app().session.active_sequence().unwrap().video_tracks[0].items.len();
    d.click("effectControls.effect.gaussian_blur");
    assert_eq!(d.app().ui.focused, filmcraft_ui_egui::dock::PanelKind::EffectControls);
    d.ok("ui.key", json!({"key": "Cmd+C"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "Cmd+V"}));
    d.frames(3);
    assert_eq!(d.blurriness(2), vec![30.0, 30.0], "Cmd+V pastes a second blur");
    assert_eq!(d.app().session.active_sequence().unwrap().video_tracks[0].items.len(), clips, "no clip is pasted");
}
