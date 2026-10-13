//! Robustness: an interchange document is a file someone hands us, so a mutated, truncated or
//! spliced EDL, XML, OTIO, AAF or OMF must import or fail with an error, never panic (AGENTS.md §0).
//! Seeds are our own exports of a small project in every format (see `filmcraft_testkit::fuzz`).

mod support;

use filmcraft_interchange::{ExportOptions, Format, detect, export, import};
use filmcraft_project::{Project, TrackKind, TransitionAlign};
use filmcraft_testkit::fuzz;
use filmcraft_time::FrameRate;
use support::*;

/// Our own export of a small project in `format`: linked video and audio, a second audio-only
/// file, a dissolve, a non-ASCII file and sequence name and a drop-frame rate.
fn seed(format: Format) -> Vec<u8> {
    let r = FrameRate::FPS_29_97;
    let mut p = Project::new("fuzz");
    let a = media(&mut p, "/media/カメラ/a 01.mov", true, true, r);
    let b = media(&mut p, "/media/b.wav", false, true, r);
    let s = sequence(&mut p, "Séquence", r, true);
    let v1 = clip(&mut p, s, TrackKind::Video, 0, a, 0, 60, 30);
    let a1 = clip(&mut p, s, TrackKind::Audio, 0, a, 0, 60, 30);
    link(&mut p, s, &[v1, a1]);
    let v2 = clip(&mut p, s, TrackKind::Video, 0, a, 60, 45, 300);
    clip(&mut p, s, TrackKind::Audio, 1, b, 15, 90, 0);
    transition(&mut p, s, TrackKind::Video, 0, "cross_dissolve", Some(v1), Some(v2), 55, 10, TransitionAlign::CenterAtCut);
    export(&p, s, format, &ExportOptions::default()).map(|(bytes, _)| bytes).unwrap_or_else(|e| panic!("seed export {format:?}: {e}"))
}

#[test]
fn mutated_documents_never_panic() {
    let mut found = Vec::new();
    for format in Format::ALL {
        let seed = seed(format);
        import(&seed, format, None).unwrap_or_else(|e| panic!("seed {format:?} imports: {e}"));
        for mut f in fuzz::run(&format!("interchange-{}", format.extension()), &[seed], 1500, |bytes| {
            let _ = import(bytes, format, Some("/base"));
            let _ = detect(bytes, Some(format.extension()));
        }) {
            f.message = format!("{}: {}", format.name(), f.message);
            found.push(f);
        }
    }
    fuzz::assert_clean("interchange", &found);
}
