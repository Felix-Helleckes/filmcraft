//! Robustness: a project file can be damaged (a full disk, a sync conflict) or edited by hand, so
//! opening a mutated `.fcproj` must open or fail with an error, never panic (AGENTS.md §0), and
//! neither may inspecting the opened project or rendering a frame of it. The seed is the demo
//! project saved by us. Byte mutations (`filmcraft_testkit::fuzz`) mostly break the JSON, so most
//! inputs here are structural ones that keep it valid: a number replaced by an extreme, a field
//! or an array element dropped, an array element repeated (`FILMCRAFT_FUZZ_PROJECTS` variants,
//! default 150).

use filmcraft_testkit::fuzz::{self, Rng};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;

const NUMBERS: &[f64] = &[0.0, -1.0, 1.0, -0.5, 1e-300, 1e308, -1e308, 4_294_967_296.0, -4_294_967_296.0, 9.3e18, -9.3e18];
const INTS: &[i64] = &[0, -1, 1, i64::MAX, i64::MIN, i64::MAX / 4 + 1, u32::MAX as i64 + 1, -(u32::MAX as i64)];

/// Every node of `v` as a path of keys / indices.
fn paths(v: &Value, at: &mut Vec<Value>, out: &mut Vec<Vec<Value>>) {
    out.push(at.clone());
    match v {
        Value::Object(m) => {
            for (k, c) in m {
                at.push(json!(k));
                paths(c, at, out);
                at.pop();
            }
        }
        Value::Array(a) => {
            for (i, c) in a.iter().enumerate() {
                at.push(json!(i));
                paths(c, at, out);
                at.pop();
            }
        }
        _ => {}
    }
}

fn node<'a>(v: &'a mut Value, path: &[Value]) -> Option<&'a mut Value> {
    path.iter().try_fold(v, |v, k| match k {
        Value::String(k) => v.get_mut(k.as_str()),
        Value::Number(i) => v.get_mut(i.as_u64()? as usize),
        _ => None,
    })
}

/// One to three structural mutations of a JSON document.
fn mutate_json(rng: &mut Rng, doc: &Value) -> Vec<u8> {
    let mut v = doc.clone();
    for _ in 0..1 + rng.below(3) {
        let mut all = Vec::new();
        paths(&v, &mut Vec::new(), &mut all);
        let Some(path) = all.get(rng.below(all.len())).cloned() else { continue };
        if path.is_empty() {
            continue;
        }
        let (parent, last) = path.split_at(path.len() - 1);
        let Some(target) = node(&mut v, &path) else { continue };
        match target {
            Value::Number(n) if n.is_f64() || rng.below(4) == 0 => *target = json!(NUMBERS[rng.below(NUMBERS.len())]),
            Value::Number(_) => *target = json!(INTS[rng.below(INTS.len())]),
            Value::String(s) => *s = ["", "日本語", "a\u{0}b"][rng.below(3)].to_string(),
            Value::Bool(b) => *b = !*b,
            _ => match (node(&mut v, parent), &last[0]) {
                (Some(Value::Object(m)), Value::String(k)) => {
                    m.remove(k.as_str());
                }
                (Some(Value::Array(a)), Value::Number(i)) => {
                    let i = i.as_u64().unwrap_or(0) as usize;
                    if rng.below(2) == 0 {
                        a.remove(i);
                    } else {
                        let c = a[i].clone();
                        a.insert(i, c);
                    }
                }
                _ => {}
            },
        }
    }
    serde_json::to_vec_pretty(&v).unwrap_or_default()
}

#[test]
fn mutated_project_files_never_panic() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let seed = filmcraft_format::encode(&s.project, true);
    let doc: Value = serde_json::from_slice(&seed).unwrap();
    let mut rng = Rng(0x5eed);
    let structural: usize = std::env::var("FILMCRAFT_FUZZ_PROJECTS").ok().and_then(|v| v.parse().ok()).unwrap_or(150);
    let mut seeds: Vec<Vec<u8>> = (0..structural).map(|_| mutate_json(&mut rng, &doc)).collect();
    seeds.push(seed);
    let dir = tmp_dir("project-fuzz");
    let path = dir.join("fuzz.fcproj");
    let path_s = path.to_string_lossy().to_string();
    // each structural variant once, and a few byte mutations of each
    let found = fuzz::run("project", &seeds, 2, |bytes| {
        if std::fs::write(&path, bytes).is_err() {
            return;
        }
        let mut t = Session::default();
        if t.execute("file.open", json!({"path": path_s})).is_err() {
            return;
        }
        let _ = t.execute("sequence.inspect", json!({}));
        let end = t.active_sequence().map_or(Tick::ZERO, |q| q.duration());
        for at in [Tick::ZERO, Tick(end.0 / 2)] {
            let _ = t.render_program_at(0.05, at);
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
    fuzz::assert_clean("project", &found);
}
