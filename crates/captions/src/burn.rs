//! Caption layout and burn-in.
//!
//! [`overlay`] lays out the caption showing on a track at a time with the track's
//! [`CaptionStyle`] (size relative to a 1080-line frame, colour, background box, outline, drop shadow,
//! alignment, anchor and margin; WebVTT `line:N%` and `align:` cue settings override the style)
//! and rasterises it into a small premultiplied linear-light RGBA [`Overlay`] positioned in the
//! frame. Renderers composite overlays over the finished picture.

use filmcraft_color::srgb_to_linear;
use filmcraft_project::{Caption, CaptionAlign, CaptionAnchor, CaptionStyle, CaptionTrack, Sequence};
use filmcraft_time::Tick;

use filmcraft_text::{ParagraphStyle, TextStyle, layout, render};

/// A rendered caption: premultiplied linear-light RGBA f32 pixels at `(x, y)` in the frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    pub x: i32,
    pub y: i32,
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

impl Overlay {
    /// Composite over a premultiplied RGBA f32 canvas of `cw`×`ch` pixels.
    pub fn composite_onto(&self, canvas: &mut [f32], cw: usize, ch: usize) {
        for y in 0..self.h {
            let cy = self.y + y as i32;
            if cy < 0 || cy >= ch as i32 {
                continue;
            }
            for x in 0..self.w {
                let cx = self.x + x as i32;
                if cx < 0 || cx >= cw as i32 {
                    continue;
                }
                let s = &self.px[(y * self.w + x) * 4..(y * self.w + x) * 4 + 4];
                if s[3] <= 0.0 {
                    continue;
                }
                let i = (cy as usize * cw + cx as usize) * 4;
                let k = 1.0 - s[3];
                for c in 0..4 {
                    canvas[i + c] = s[c] + canvas[i + c] * k;
                }
            }
        }
    }
}

/// The caption face: the track's font family (Inter when empty or not installed) in SemiBold at `px`.
fn caption_style(font: &str, px: f32) -> TextStyle {
    let family = if font.trim().is_empty() { filmcraft_text::fonts::DEFAULT_FAMILY } else { font.trim() };
    TextStyle { family: family.into(), style: "SemiBold".into(), size: px, ..Default::default() }
}

fn lin(c: [u8; 4]) -> [f32; 4] {
    let a = c[3] as f32 / 255.0;
    [srgb_to_linear(c[0] as f32 / 255.0) * a, srgb_to_linear(c[1] as f32 / 255.0) * a, srgb_to_linear(c[2] as f32 / 255.0) * a, a]
}

/// Word-wrap one line to `max_w` pixels.
fn wrap(line: &str, ts: &TextStyle, max_w: f32) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in line.split(' ') {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if !cur.is_empty() && filmcraft_text::measure(&cand, ts) > max_w {
            out.push(std::mem::take(&mut cur));
            cur = word.to_string();
        } else {
            cur = cand;
        }
    }
    out.push(cur);
    out
}

/// Placement overrides from WebVTT cue settings.
fn vtt_overrides(settings: &str) -> (Option<f32>, Option<CaptionAlign>) {
    let mut line = None;
    let mut align = None;
    for kv in settings.split_whitespace() {
        let Some((k, v)) = kv.split_once(':') else { continue };
        match k {
            "line" => {
                let v = v.split(',').next().unwrap_or("");
                if let Some(p) = v.strip_suffix('%').and_then(|p| p.parse::<f32>().ok()) {
                    line = Some((p / 100.0).clamp(0.0, 1.0));
                }
            }
            "align" => {
                align = match v {
                    "start" | "left" => Some(CaptionAlign::Left),
                    "end" | "right" => Some(CaptionAlign::Right),
                    "center" | "middle" => Some(CaptionAlign::Center),
                    _ => None,
                }
            }
            _ => {}
        }
    }
    (line, align)
}

/// Lay out and rasterise one caption for a `w`×`h` frame.
pub fn render_caption(c: &Caption, style: &CaptionStyle, w: usize, h: usize) -> Option<Overlay> {
    if w == 0 || h == 0 {
        return None;
    }
    let scale = h as f32 / 1080.0;
    let px = (style.size * scale).max(4.0);
    let max_w = w as f32 * 0.9;
    let ts = caption_style(&style.font, px);
    let mut lines: Vec<String> = Vec::new();
    // (speaker names are metadata; like Premiere, they are not burned in)
    for l in c.plain_lines() {
        lines.extend(wrap(l.trim(), &ts, max_w));
    }
    lines.retain(|l| !l.is_empty());
    if lines.is_empty() {
        return None;
    }
    let vm = filmcraft_text::fonts::face(filmcraft_text::resolve(&ts.family, &ts.style).face).metrics(px);
    let (asc, desc) = (vm.ascent, vm.descent);
    let lh = px * style.line_spacing.max(0.8);
    let pad_x = (px * 0.3).round();
    let block_h = lh * lines.len() as f32;
    let (line_override, align_override) = vtt_overrides(&c.settings);
    let align = align_override.unwrap_or(style.align);
    let margin = style.margin.clamp(0.0, 0.45) * h as f32;
    let top = match (line_override, style.anchor) {
        (Some(f), _) => (f * h as f32).min(h as f32 - block_h),
        (None, CaptionAnchor::Top) => margin,
        (None, CaptionAnchor::Middle) => (h as f32 - block_h) / 2.0,
        (None, CaptionAnchor::Bottom) => h as f32 - margin - block_h,
    }
    .max(0.0)
    .round();
    let side = w as f32 * 0.05;
    let widths: Vec<f32> = lines.iter().map(|l| filmcraft_text::measure(l, &ts)).collect();
    let xs: Vec<f32> = widths
        .iter()
        .map(|&lw| match align {
            CaptionAlign::Left => side + pad_x,
            CaptionAlign::Right => w as f32 - side - pad_x - lw,
            CaptionAlign::Center => (w as f32 - lw) / 2.0,
        })
        .collect();
    let outline = (style.outline * scale).max(0.0);
    let shadow = shadow_geometry(style, scale);
    // the overlay also holds the shadow: its offset, spread and blur on every side
    let grow = outline.ceil() + 1.0 + shadow.map_or(0.0, |s| (s.dx.abs().max(s.dy.abs()) + s.size + s.blur).ceil());
    let bx0 = xs.iter().zip(&widths).map(|(x, _)| x - pad_x - grow).fold(f32::MAX, f32::min).floor();
    let bx1 = xs.iter().zip(&widths).map(|(x, lw)| x + lw + pad_x + grow).fold(f32::MIN, f32::max).ceil();
    let by0 = (top - grow).floor();
    let by1 = (top + block_h + grow).ceil();
    let (ox, oy) = (bx0 as i32, by0 as i32);
    let ow = (bx1 - bx0).max(1.0) as usize;
    let oh = (by1 - by0).max(1.0) as usize;
    let mut cover = vec![0.0f32; ow * oh];
    let mut bg = vec![0.0f32; ow * oh];
    for (i, line) in lines.iter().enumerate() {
        let lt = top + lh * i as f32;
        if style.background && style.background_color[3] > 0 {
            let (x0, x1) = (xs[i] - pad_x, xs[i] + widths[i] + pad_x);
            let (y0, y1) = (lt, lt + lh);
            for y in (y0.floor() as i32).max(oy)..(y1.ceil() as i32).min(oy + oh as i32) {
                let fy = ((y as f32 + 1.0).min(y1) - (y as f32).max(y0)).clamp(0.0, 1.0);
                for x in (x0.floor() as i32).max(ox)..(x1.ceil() as i32).min(ox + ow as i32) {
                    let fx = ((x as f32 + 1.0).min(x1) - (x as f32).max(x0)).clamp(0.0, 1.0);
                    let j = (y - oy) as usize * ow + (x - ox) as usize;
                    bg[j] = (bg[j] + fx * fy).min(1.0);
                }
            }
        }
        let baseline = lt + (lh - (asc + desc)) / 2.0 + asc;
        let l = layout(line, &ts, &ParagraphStyle::default());
        let mut m = filmcraft_text::Mask { w: ow, h: oh, a: std::mem::take(&mut cover) };
        render::draw(&l, &render::at(xs[i], baseline.round()), &mut m, (ox, oy));
        cover = m.a;
    }
    let stroke = if outline > 0.0 { dilate(&cover, ow, oh, outline) } else { Vec::new() };
    let shade = shadow.map(|s| shadow_mask(&cover, &stroke, ow, oh, s)).unwrap_or_default();
    let (tc, bc, oc) = (lin(style.color), lin(style.background_color), lin(style.outline_color));
    let sc = shadow.map_or([0.0; 4], |s| lin(style.shadow_color).map(|c| c * s.opacity));
    let mut out = vec![0.0f32; ow * oh * 4];
    for j in 0..ow * oh {
        let mut p = [0.0f32; 4];
        let over = |p: &mut [f32; 4], c: [f32; 4], a: f32| {
            if a > 0.0 {
                for k in 0..4 {
                    p[k] = c[k] * a + p[k] * (1.0 - c[3] * a);
                }
            }
        };
        over(&mut p, bc, bg[j]);
        if let Some(&a) = shade.get(j) {
            over(&mut p, sc, a);
        }
        if !stroke.is_empty() {
            over(&mut p, oc, stroke[j]);
        }
        over(&mut p, tc, cover[j]);
        out[j * 4..j * 4 + 4].copy_from_slice(&p);
    }
    Some(Overlay { x: ox, y: oy, w: ow, h: oh, px: out })
}

/// A caption shadow in overlay pixels: offset, spread, blur radius and opacity (0–1).
#[derive(Clone, Copy, Debug, PartialEq)]
struct ShadowGeometry {
    dx: f32,
    dy: f32,
    size: f32,
    blur: f32,
    opacity: f32,
}

/// The track's shadow at `scale` (frame height / 1080), or None when it is off or invisible. The
/// values come from project files and commands, so each is bounded here: a NaN or a huge distance
/// must not size the overlay.
fn shadow_geometry(style: &CaptionStyle, scale: f32) -> Option<ShadowGeometry> {
    let bounded = |v: f32, max: f32| if v.is_finite() { v.clamp(0.0, max) } else { 0.0 };
    let opacity = bounded(style.shadow_opacity, 100.0) / 100.0;
    if !style.shadow || opacity <= 0.0 || style.shadow_color[3] == 0 {
        return None;
    }
    // Premiere (and graphic text): 135° casts the shadow down and to the right.
    let angle = if style.shadow_angle.is_finite() { (style.shadow_angle - 90.0).to_radians() } else { std::f32::consts::FRAC_PI_4 };
    let d = bounded(style.shadow_distance, 200.0) * scale;
    Some(ShadowGeometry {
        dx: angle.cos() * d,
        dy: angle.sin() * d,
        size: bounded(style.shadow_size, 100.0) * scale,
        blur: bounded(style.shadow_blur, 200.0) * scale,
        opacity,
    })
}

/// The shadow's coverage: the text with its outline, grown by `size`, softened by `blur` and moved
/// by the offset (sampled bilinearly, so a fractional distance stays smooth).
fn shadow_mask(cover: &[f32], stroke: &[f32], w: usize, h: usize, s: ShadowGeometry) -> Vec<f32> {
    let mut m: Vec<f32> = cover.iter().enumerate().map(|(j, &c)| c.max(stroke.get(j).copied().unwrap_or(0.0))).collect();
    // both in time linear in the overlay, whatever the size and blur (a disc or Gaussian kernel of
    // the allowed radii would cost hundreds of taps per pixel)
    if s.size > 0.0 {
        m = spread(&m, w, h, s.size);
    }
    if s.blur > 0.0 {
        m = soften(&m, w, h, s.blur);
    }
    let at = |x: i64, y: i64| -> f32 {
        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
            return 0.0;
        }
        m.get(y as usize * w + x as usize).copied().unwrap_or(0.0)
    };
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = (x as f32 - s.dx, y as f32 - s.dy);
            let (x0, y0) = (sx.floor(), sy.floor());
            let (fx, fy) = (sx - x0, sy - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
            let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
            if let Some(v) = out.get_mut(y * w + x) {
                *v = (top * (1.0 - fy) + bottom * fy).clamp(0.0, 1.0);
            }
        }
    }
    out
}

/// Grow a coverage mask by `r` pixels with round corners: the distance from each pixel to the
/// shape (an exact Euclidean distance transform of the half-covered pixels) against `r`, with a
/// one-pixel ramp so the edge stays anti-aliased.
fn spread(m: &[f32], w: usize, h: usize, r: f32) -> Vec<f32> {
    let d2 = distance_squared(m, w, h);
    m.iter().zip(&d2).map(|(&c, &d)| c.max((r + 0.5 - d.sqrt()).clamp(0.0, 1.0))).collect()
}

/// Squared distance to the nearest pixel with coverage ≥ 0.5 (infinite for an empty mask), by
/// Felzenszwalb and Huttenlocher's two-pass lower-envelope method.
fn distance_squared(m: &[f32], w: usize, h: usize) -> Vec<f32> {
    const FAR: f32 = 1e20;
    let mut g: Vec<f32> = m.iter().map(|&c| if c >= 0.5 { 0.0 } else { FAR }).collect();
    let mut line = Vec::new();
    for x in 0..w {
        line.clear();
        line.extend((0..h).map(|y| g.get(y * w + x).copied().unwrap_or(FAR)));
        for (y, v) in edt_1d(&line).into_iter().enumerate() {
            if let Some(d) = g.get_mut(y * w + x) {
                *d = v;
            }
        }
    }
    for y in 0..h {
        let row = g.get(y * w..(y + 1) * w).map(<[f32]>::to_vec).unwrap_or_default();
        for (x, v) in edt_1d(&row).into_iter().enumerate() {
            if let Some(d) = g.get_mut(y * w + x) {
                *d = v;
            }
        }
    }
    g
}

/// One dimension of the distance transform: `out[q] = min_p (q - p)² + f[p]`.
fn edt_1d(f: &[f32]) -> Vec<f32> {
    let n = f.len();
    let mut out = vec![1e20f32; n];
    // parabola vertices and the boundaries between them
    let mut v = vec![0usize; n.max(1)];
    let mut z = vec![0.0f32; n + 1];
    let mut k = 0usize;
    let value = |q: usize| f.get(q).copied().unwrap_or(1e20);
    let Some(first) = (0..n).find(|&q| value(q) < 1e20) else { return out };
    v[0] = first;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in first + 1..n {
        if value(q) >= 1e20 {
            continue;
        }
        loop {
            let p = v[k];
            let s = ((value(q) + (q * q) as f32) - (value(p) + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
            if s <= z[k] && k > 0 {
                k -= 1;
                continue;
            }
            if s <= z[k] {
                // k == 0: the new parabola is lower everywhere
                v[0] = q;
                z[1] = f32::INFINITY;
                break;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
            break;
        }
    }
    let mut j = 0usize;
    for (q, o) in out.iter_mut().enumerate() {
        while z.get(j + 1).is_some_and(|&b| b < q as f32) {
            j += 1;
        }
        let p = v[j];
        let d = q as f32 - p as f32;
        *o = d * d + value(p);
    }
    out
}

/// Soften a coverage mask: three box blurs in each direction, close to a Gaussian with σ = r / 2,
/// in time independent of `r` (running sums). Edges read as empty.
fn soften(m: &[f32], w: usize, h: usize, r: f32) -> Vec<f32> {
    let sigma = r / 2.0;
    // the box half-width whose three passes have the Gaussian's variance: 3·((2b+1)² − 1)/12 = σ²
    let b = (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0).round().max(1.0) as usize;
    let mut cur = m.to_vec();
    let mut line: Vec<f32> = Vec::new();
    for _ in 0..3 {
        for y in 0..h {
            line.clear();
            line.extend((0..w).map(|x| cur.get(y * w + x).copied().unwrap_or(0.0)));
            for (x, v) in box_1d(&line, b).into_iter().enumerate() {
                if let Some(d) = cur.get_mut(y * w + x) {
                    *d = v;
                }
            }
        }
        for x in 0..w {
            line.clear();
            line.extend((0..h).map(|y| cur.get(y * w + x).copied().unwrap_or(0.0)));
            for (y, v) in box_1d(&line, b).into_iter().enumerate() {
                if let Some(d) = cur.get_mut(y * w + x) {
                    *d = v;
                }
            }
        }
    }
    cur
}

/// Mean over `[i - b, i + b]` (outside the line counts as 0), by a running sum.
fn box_1d(f: &[f32], b: usize) -> Vec<f32> {
    let n = f.len();
    let at = |i: isize| if i >= 0 { f.get(i as usize).copied().unwrap_or(0.0) } else { 0.0 };
    let width = (2 * b + 1) as f32;
    let mut sum: f32 = (0..=b as isize).map(at).sum();
    let mut out = Vec::with_capacity(n);
    for i in 0..n as isize {
        out.push(sum / width);
        sum += at(i + b as isize + 1) - at(i - b as isize);
    }
    out
}

/// Max-filter a coverage mask with a disc of radius `r`.
fn dilate(m: &[f32], w: usize, h: usize, r: f32) -> Vec<f32> {
    let ri = r.ceil() as i32;
    let mut offs = Vec::new();
    for dy in -ri..=ri {
        for dx in -ri..=ri {
            let d = ((dx * dx + dy * dy) as f32).sqrt();
            if d <= r + 0.5 {
                offs.push((dx, dy, (r + 0.5 - d).clamp(0.0, 1.0)));
            }
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut v = 0.0f32;
            for &(dx, dy, wgt) in &offs {
                let (sx, sy) = (x + dx, y + dy);
                if sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 {
                    v = v.max(m[sy as usize * w + sx as usize] * wgt);
                }
            }
            out[y as usize * w + x as usize] = v;
        }
    }
    out
}

/// The overlay for a caption track at timeline time `t` (None when nothing shows).
pub fn track_overlay(track: &CaptionTrack, t: Tick, w: usize, h: usize) -> Option<Overlay> {
    if !track.enabled {
        return None;
    }
    let c = track.caption_at(t)?;
    render_caption(c, &track.style, w, h)
}

/// Overlays of every visible caption track of a sequence at `t`, bottom track first.
pub fn sequence_overlays(seq: &Sequence, t: Tick, w: usize, h: usize) -> Vec<Overlay> {
    seq.caption_tracks.iter().rev().filter_map(|tr| track_overlay(tr, t, w, h)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{CaptionFormat, ClipId, TrackId};

    fn cap(text: &str) -> Caption {
        Caption { id: ClipId(1), start: Tick(0), duration: Tick(100), text: text.into(), speaker: None, cue_id: None, settings: String::new() }
    }

    #[test]
    fn bottom_centre_with_box() {
        let st = CaptionStyle::default();
        let o = render_caption(&cap("Hello world"), &st, 1920, 1080).unwrap();
        // centred horizontally, in the lower part of the frame
        let cx = o.x as f32 + o.w as f32 / 2.0;
        assert!((cx - 960.0).abs() < 4.0, "centre {cx}");
        assert!(o.y > 800 && (o.y as usize + o.h) < 1080, "y {} h {}", o.y, o.h);
        // white text pixels exist and the box is dark translucent
        let mut white = 0;
        let mut boxy = 0;
        for p in o.px.chunks(4) {
            if p[3] > 0.99 && p[0] > 0.9 {
                white += 1;
            } else if p[3] > 0.7 && p[0] < 0.01 {
                boxy += 1;
            }
        }
        assert!(white > 500, "{white}");
        assert!(boxy > 1000, "{boxy}");
    }

    #[test]
    fn top_anchor_and_vtt_line() {
        let st = CaptionStyle { anchor: CaptionAnchor::Top, ..Default::default() };
        let o = render_caption(&cap("Top"), &st, 1280, 720).unwrap();
        assert!(o.y < 100);
        let mut c = cap("Line");
        c.settings = "line:50% align:start".into();
        let o = render_caption(&c, &CaptionStyle::default(), 1280, 720).unwrap();
        assert!((o.y - 360).abs() < 4, "{}", o.y);
        assert!(o.x < 120, "{}", o.x);
    }

    #[test]
    fn margin_is_a_fraction_of_frame_height() {
        // #1542: on a portrait sequence, a bottom-anchored caption with margin 0.31 sits with
        // its bottom edge 0.31 of the frame height above the bottom; the top anchor mirrors it.
        let st = CaptionStyle { anchor: CaptionAnchor::Bottom, margin: 0.31, size: 78.0, ..Default::default() };
        let o = render_caption(&cap("Hello world"), &st, 1080, 1920).unwrap();
        // the overlay pads the content by the outline grow (the default outline is 0, so by 1 px)
        let bottom = (o.y as usize + o.h) as f32;
        let expected = 1920.0 - 0.31 * 1920.0;
        assert!((bottom - expected).abs() <= 2.0, "bottom edge {bottom}, expected {expected}");
        let st = CaptionStyle { anchor: CaptionAnchor::Top, margin: 0.31, size: 78.0, ..Default::default() };
        let o = render_caption(&cap("Hello world"), &st, 1080, 1920).unwrap();
        assert!((o.y as f32 - 0.31 * 1920.0).abs() <= 2.0, "top edge {}, expected {}", o.y, 0.31 * 1920.0);
        // margin 0 and the 0.45 clamp stay inside the frame
        for margin in [0.0, 0.31, 0.45, 9.0] {
            let st = CaptionStyle { anchor: CaptionAnchor::Bottom, margin, size: 78.0, ..Default::default() };
            let o = render_caption(&cap("Hello world"), &st, 1080, 1920).unwrap();
            // the outline-grow padding plus ceil rounding may reach 2 px past the edge; the
            // composite clips it
            assert!(o.y >= 0 && o.y as usize + o.h <= 1922, "margin {margin}: y {} h {}", o.y, o.h);
        }
    }

    #[test]
    fn wraps_and_composites() {
        let long = "word ".repeat(60);
        let o = render_caption(&cap(&long), &CaptionStyle::default(), 640, 360).unwrap();
        assert!(o.w <= 640 && o.h > 40, "{}x{}", o.w, o.h);
        let mut canvas = vec![0.0f32; 640 * 360 * 4];
        o.composite_onto(&mut canvas, 640, 360);
        assert!(canvas.chunks(4).any(|p| p[0] > 0.9));
    }

    #[test]
    fn outline_draws_around_text() {
        let st = CaptionStyle { background: false, outline: 4.0, outline_color: [255, 0, 0, 255], ..Default::default() };
        let o = render_caption(&cap("O"), &st, 1920, 1080).unwrap();
        assert!(o.px.chunks(4).any(|p| p[0] > 0.9 && p[1] < 0.05 && p[3] > 0.9), "red outline pixels");
    }

    /// Opaque pixels of an overlay as frame coordinates, with their premultiplied colour.
    fn opaque(o: &Overlay) -> Vec<(i32, i32, [f32; 4])> {
        (0..o.h)
            .flat_map(|y| (0..o.w).map(move |x| (x, y)))
            .filter_map(|(x, y)| {
                let p = o.px.get((y * o.w + x) * 4..(y * o.w + x) * 4 + 4)?;
                (p[3] > 0.5).then(|| (o.x + x as i32, o.y + y as i32, [p[0], p[1], p[2], p[3]]))
            })
            .collect()
    }

    #[test]
    fn drop_shadow_falls_down_and_right_behind_the_text() {
        let plain = CaptionStyle { background: false, ..Default::default() };
        let off = render_caption(&cap("I"), &plain, 1920, 1080).unwrap();
        // shadow off (the default) draws exactly what it drew before the shadow existed
        let toggled_off = CaptionStyle { shadow: false, shadow_distance: 50.0, shadow_blur: 80.0, ..plain.clone() };
        assert_eq!(render_caption(&cap("I"), &toggled_off, 1920, 1080).unwrap(), off);
        let st = CaptionStyle { shadow: true, shadow_color: [255, 0, 0, 255], shadow_opacity: 100.0, shadow_distance: 12.0, shadow_blur: 0.0, ..plain };
        let on = render_caption(&cap("I"), &st, 1920, 1080).unwrap();
        let red: Vec<_> = opaque(&on).into_iter().filter(|(_, _, p)| p[0] > 0.9 && p[1] < 0.05).collect();
        assert!(!red.is_empty(), "red shadow pixels");
        let text = opaque(&off);
        let centre = |v: &[(i32, i32, [f32; 4])]| {
            let n = v.len().max(1) as f32;
            (v.iter().map(|p| p.0 as f32).sum::<f32>() / n, v.iter().map(|p| p.1 as f32).sum::<f32>() / n)
        };
        let ((tx, ty), (sx, sy)) = (centre(&text), centre(&red));
        // 135° at 12 px: about 8.5 px right and 8.5 px down (minus what the text covers)
        assert!(sx > tx + 4.0 && sy > ty + 4.0, "text at {tx},{ty}, shadow at {sx},{sy}");
        // the text stays on top: where it fully covers, it is still white (its anti-aliased edge
        // blends with the shadow behind it, as it would with anything behind it)
        let at = |o: &Overlay, x: i32, y: i32| {
            let (lx, ly) = ((x - o.x) as usize, (y - o.y) as usize);
            o.px.get((ly * o.w + lx) * 4..(ly * o.w + lx) * 4 + 4).map(|p| [p[0], p[1], p[2], p[3]]).unwrap_or([0.0; 4])
        };
        let solid: Vec<_> = text.iter().filter(|(_, _, p)| p[3] > 0.99).collect();
        assert!(!solid.is_empty());
        assert!(solid.iter().all(|(x, y, _)| at(&on, *x, *y)[1] > 0.95), "the shadow is drawn over the text");
    }

    #[test]
    fn shadow_spread_is_round_and_blur_keeps_the_coverage() {
        // one covered pixel in the middle of a 21×21 mask
        let (w, h) = (21usize, 21usize);
        let mut m = vec![0.0f32; w * h];
        m[10 * w + 10] = 1.0;
        let s = spread(&m, w, h, 4.0);
        let at = |v: &[f32], x: usize, y: usize| v[y * w + x];
        assert_eq!(at(&s, 14, 10), 0.5, "4 px out along an axis: on the anti-aliased edge");
        assert_eq!(at(&s, 13, 10), 1.0);
        assert_eq!(at(&s, 16, 10), 0.0);
        // round, not square: the diagonal at distance 4 (≈ 2.83, 2.83) is in, the corner (4, 4) is out
        assert!(at(&s, 13, 13) > 0.0 && at(&s, 14, 14) == 0.0);
        // the blur spreads the coverage without losing it (nothing reaches the mask's edges)
        let b = soften(&s, w, h, 3.0);
        let (before, after): (f32, f32) = (s.iter().sum(), b.iter().sum());
        assert!((before - after).abs() < 1e-3 * before, "{before} → {after}");
        assert!(at(&b, 10, 10) < 1.0 && at(&b, 16, 10) > 0.0);
        // an empty mask stays empty
        assert!(spread(&vec![0.0; w * h], w, h, 3.0).iter().all(|v| *v == 0.0));
    }

    #[test]
    fn drop_shadow_values_from_a_file_are_bounded() {
        for (d, s, b) in [(f32::NAN, f32::INFINITY, f32::NAN), (1e9, 1e9, 1e9), (-50.0, -1.0, -3.0)] {
            let st = CaptionStyle { shadow: true, shadow_distance: d, shadow_size: s, shadow_blur: b, shadow_angle: f32::NAN, ..Default::default() };
            let o = render_caption(&cap("Hi"), &st, 1920, 1080).unwrap();
            assert!(o.w < 1920 + 1200 && o.h < 1080 + 1200, "{d} {s} {b}: {}x{}", o.w, o.h);
            assert!(o.px.iter().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn track_font_is_used() {
        // the style's font used to be ignored (always Inter)
        let inter = render_caption(&cap("Hello world"), &CaptionStyle::default(), 1920, 1080).unwrap();
        let serif = CaptionStyle { font: "Noto Serif".into(), ..Default::default() };
        let serif = render_caption(&cap("Hello world"), &serif, 1920, 1080).unwrap();
        assert_ne!((inter.w, &inter.px), (serif.w, &serif.px), "Noto Serif renders like Inter");
        let mono = CaptionStyle { font: "jetbrains mono".into(), ..Default::default() };
        let mono = render_caption(&cap("Hello world"), &mono, 1920, 1080).unwrap();
        assert_ne!((inter.w, &inter.px), (mono.w, &mono.px), "JetBrains Mono renders like Inter");
    }

    #[test]
    fn unknown_or_empty_font_falls_back_to_inter() {
        let inter = render_caption(&cap("Hello"), &CaptionStyle::default(), 1280, 720).unwrap();
        for font in ["", "  ", "No Such Font 123"] {
            let st = CaptionStyle { font: font.into(), ..Default::default() };
            assert_eq!(render_caption(&cap("Hello"), &st, 1280, 720), Some(inter.clone()), "font {font:?}");
        }
    }

    #[test]
    fn hidden_track_draws_nothing() {
        let mut t = CaptionTrack::new(TrackId(1), "S".into(), CaptionFormat::Subtitle);
        t.captions.push(cap("x"));
        assert!(track_overlay(&t, Tick(5), 100, 100).is_some());
        t.enabled = false;
        assert!(track_overlay(&t, Tick(5), 100, 100).is_none());
    }
}
