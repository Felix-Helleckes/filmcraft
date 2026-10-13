//! Robustness: a caption file is something someone hands us, so a mutated, truncated or spliced
//! SRT, WebVTT, SCC, MCC, EBU STL, TTML or DFXP must parse or fail with an error, never panic
//! (AGENTS.md §0), and neither may laying out and burning in what it parsed. Seeds are our own
//! writes of a small document in every format (see `filmcraft_testkit::fuzz`).

use filmcraft_captions::{Cue, Document, Format, WriteOptions, burn, parse, track_from_document, write};
use filmcraft_project::{CaptionStyle, TrackId};
use filmcraft_testkit::fuzz;
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};

fn doc() -> Document {
    let at = |s: f64| Tick((s * TICKS_PER_SECOND as f64) as i64);
    let cues = vec![
        Cue { start: at(1.0), end: at(2.5), text: "Hello, world".into(), ..Default::default() },
        Cue {
            start: at(3.0),
            end: at(4.0),
            text: "<i>Liftoff</i> — 発射\nsecond line".into(),
            speaker: Some("Mission Control".into()),
            id: Some("c2".into()),
            settings: "line:80% align:start".into(),
        },
        Cue { start: at(4.2), end: at(6.0), text: "Ça va? 100% <b>bold</b> & co.".into(), ..Default::default() },
    ];
    Document { cues, blocks: vec!["STYLE\n::cue { color: lime }".into()], warnings: Vec::new() }
}

#[test]
fn mutated_caption_files_never_panic() {
    let doc = doc();
    let mut found = Vec::new();
    for format in Format::ALL {
        let seed = write(&doc, format, WriteOptions { drop_frame: true, rate: Some(FrameRate::FPS_29_97) });
        parse(&seed, format).unwrap_or_else(|e| panic!("seed {format:?} parses: {e}"));
        let style = CaptionStyle { outline: 2.0, background: true, ..Default::default() };
        for mut f in fuzz::run(&format!("captions-{}", format.extension()), &[seed], 1500, |bytes| {
            let _ = filmcraft_captions::detect(bytes, Some(format.extension()));
            let Ok(mut d) = parse(bytes, format) else { return };
            d.normalize();
            d.snap_to_frames(FrameRate::FPS_29_97);
            let mut next = 1u64;
            let track = track_from_document(&d, TrackId(1), "C1", format.track_format(), &mut || {
                next += 1;
                next
            });
            for c in track.captions.iter().take(4) {
                let _ = burn::render_caption(c, &style, 96, 54);
            }
            // MCC writes a packet for every frame up to the last caption, so a corrupt timecode
            // hundreds of hours out is a gigabyte-sized file by design: write back only what a
            // real caption file holds.
            if d.cues.iter().all(|c| c.end < Tick(3600 * TICKS_PER_SECOND)) {
                let _ = write(&d, format, WriteOptions::default());
            }
        }) {
            f.message = format!("{}: {}", format.name(), f.message);
            found.push(f);
        }
    }
    fuzz::assert_clean("captions", &found);
}
