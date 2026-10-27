// Looks exported from snaplite (`snaplite-looks.json`), and a port of
// snaplite's pixel pipeline (`engine.js`) so they can be applied to images
// while browsing. Geometry (rotate/crop) is not part of a look.
//
// Per pixel: lens blur -> structure/sharpening -> ambience -> tone (white
// balance, brightness, contrast, shadows/highlights, fade, curves) ->
// saturation -> filter -> vignette -> grain. Several looks compose by applying
// each one's pipeline to the previous one's output.

use serde::Deserialize;
use std::path::Path;
use std::thread;

type Points = Vec<[f32; 2]>;

fn identity() -> Points {
    vec![[0.0, 0.0], [1.0, 1.0]]
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Detail {
    pub structure: f32,
    pub sharpening: f32,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Basic {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub ambience: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub warmth: f32,
    pub tint: f32,
    pub fade: f32,
    pub vignette: f32,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Curves {
    pub rgb: Points,
    pub r: Points,
    pub g: Points,
    pub b: Points,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Blur {
    pub amount: f32,
    pub shape: String,
    pub cx: f32,
    pub cy: f32,
    pub size: f32,
    pub transition: f32,
    pub angle: f32,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Grain {
    pub amount: f32,
    pub size: f32,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct FilterChoice {
    pub id: String,
    pub strength: f32,
}

/// Every setting of a snaplite edit except geometry. Missing fields take
/// snaplite's defaults, so looks from older exports keep loading.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct LookState {
    pub detail: Detail,
    pub basic: Basic,
    pub curves: Curves,
    pub blur: Blur,
    pub grain: Grain,
    pub filter: FilterChoice,
}

macro_rules! zero_default {
    ($t:ident { $($f:ident),* }) => {
        impl Default for $t { fn default() -> Self { Self { $($f: 0.0),* } } }
    };
}
zero_default!(Detail { structure, sharpening });
zero_default!(Basic {
    brightness, contrast, saturation, ambience, highlights, shadows, warmth, tint, fade, vignette
});

impl Default for Curves {
    fn default() -> Self {
        Self { rgb: identity(), r: identity(), g: identity(), b: identity() }
    }
}
impl Default for Blur {
    fn default() -> Self {
        Self {
            amount: 0.0,
            shape: "radial".into(),
            cx: 0.5,
            cy: 0.5,
            size: 30.0,
            transition: 30.0,
            angle: 0.0,
        }
    }
}
impl Default for Grain {
    fn default() -> Self {
        Self { amount: 0.0, size: 30.0 }
    }
}
impl Default for FilterChoice {
    fn default() -> Self {
        Self { id: "none".into(), strength: 100.0 }
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct Look {
    pub name: String,
    #[serde(default)]
    pub state: LookState,
}

/// Either snaplite's "Export looks" file or a bare array of looks.
#[derive(Deserialize)]
#[serde(untagged)]
enum LooksFile {
    Wrapped { looks: Vec<Look> },
    Bare(Vec<Look>),
}

pub fn load_looks(path: &Path) -> Result<Vec<Look>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file: LooksFile =
        serde_json::from_str(&text).map_err(|e| format!("not a snaplite looks file: {e}"))?;
    Ok(match file {
        LooksFile::Wrapped { looks } | LooksFile::Bare(looks) => looks,
    })
}

// ---------- built-in nostalgia presets (snaplite FILTERS) ----------

struct Preset {
    id: &'static str,
    sat: f32,
    vig: f32,
    grain: f32,
    r: &'static [[f32; 2]],
    g: &'static [[f32; 2]],
    b: &'static [[f32; 2]],
}

const PRESETS: &[Preset] = &[
    Preset { id: "gold", sat: 1.1, vig: 0.15, grain: 0.12,
        r: &[[0., 0.02], [0.5, 0.56], [1., 1.]], g: &[[0., 0.01], [0.5, 0.52], [1., 0.97]], b: &[[0., 0.04], [0.5, 0.44], [1., 0.86]] },
    Preset { id: "instant", sat: 0.85, vig: 0.25, grain: 0.08,
        r: &[[0., 0.10], [0.25, 0.30], [0.75, 0.82], [1., 0.97]], g: &[[0., 0.08], [0.5, 0.53], [1., 0.95]], b: &[[0., 0.14], [0.5, 0.50], [1., 0.84]] },
    Preset { id: "seventies", sat: 0.8, vig: 0.3, grain: 0.2,
        r: &[[0., 0.12], [0.5, 0.60], [1., 0.98]], g: &[[0., 0.08], [0.5, 0.52], [1., 0.90]], b: &[[0., 0.10], [0.5, 0.40], [1., 0.72]] },
    Preset { id: "faded", sat: 0.6, vig: 0.1, grain: 0.15,
        r: &[[0., 0.16], [0.5, 0.55], [1., 0.92]], g: &[[0., 0.15], [0.5, 0.54], [1., 0.91]], b: &[[0., 0.18], [0.5, 0.53], [1., 0.88]] },
    Preset { id: "disposable", sat: 1.15, vig: 0.4, grain: 0.25,
        r: &[[0., 0.03], [0.3, 0.27], [0.7, 0.80], [1., 1.]], g: &[[0., 0.04], [0.3, 0.29], [0.7, 0.78], [1., 0.99]], b: &[[0., 0.08], [0.3, 0.30], [0.7, 0.72], [1., 0.93]] },
    Preset { id: "super8", sat: 0.9, vig: 0.5, grain: 0.35,
        r: &[[0., 0.08], [0.5, 0.58], [1., 1.]], g: &[[0., 0.05], [0.5, 0.50], [1., 0.93]], b: &[[0., 0.02], [0.5, 0.38], [1., 0.78]] },
    Preset { id: "expired", sat: 0.75, vig: 0.2, grain: 0.3,
        r: &[[0., 0.06], [0.5, 0.46], [1., 0.90]], g: &[[0., 0.10], [0.5, 0.53], [1., 0.95]], b: &[[0., 0.14], [0.5, 0.55], [1., 0.93]] },
    Preset { id: "cross", sat: 1.25, vig: 0.2, grain: 0.1,
        r: &[[0., 0.], [0.25, 0.18], [0.75, 0.86], [1., 1.]], g: &[[0., 0.], [0.25, 0.22], [0.75, 0.84], [1., 0.98]], b: &[[0., 0.12], [0.5, 0.46], [1., 0.80]] },
    Preset { id: "sepia", sat: 0.0, vig: 0.3, grain: 0.2,
        r: &[[0., 0.08], [0.5, 0.60], [1., 1.]], g: &[[0., 0.05], [0.5, 0.50], [1., 0.94]], b: &[[0., 0.03], [0.5, 0.38], [1., 0.80]] },
    Preset { id: "noir", sat: 0.0, vig: 0.35, grain: 0.3,
        r: &[[0., 0.], [0.25, 0.16], [0.75, 0.86], [1., 1.]], g: &[[0., 0.], [0.25, 0.16], [0.75, 0.86], [1., 1.]], b: &[[0., 0.], [0.25, 0.16], [0.75, 0.86], [1., 1.]] },
    Preset { id: "cyano", sat: 0.0, vig: 0.2, grain: 0.15,
        r: &[[0., 0.04], [0.5, 0.36], [1., 0.90]], g: &[[0., 0.10], [0.5, 0.50], [1., 0.96]], b: &[[0., 0.20], [0.5, 0.62], [1., 1.]] },
];

// ---------- pipeline ----------

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

/// Monotone cubic (Fritsch-Carlson) through sorted points in 0..1, sampled at
/// 256 positions; flat outside the first and last point.
fn curve_lut(pts: &[[f32; 2]]) -> [f32; 256] {
    let n = pts.len();
    let mut lut = [0.0f32; 256];
    if n < 2 {
        lut.fill(pts.first().map_or(0.0, |p| p[1]));
        return lut;
    }
    let mut dx = vec![0.0f32; n - 1];
    let mut m = vec![0.0f32; n - 1];
    let mut t = vec![0.0f32; n];
    for i in 0..n - 1 {
        let d = pts[i + 1][0] - pts[i][0];
        dx[i] = if d == 0.0 { 1e-6 } else { d };
        m[i] = (pts[i + 1][1] - pts[i][1]) / dx[i];
    }
    t[0] = m[0];
    t[n - 1] = m[n - 2];
    for i in 1..n - 1 {
        t[i] = if m[i - 1] * m[i] <= 0.0 { 0.0 } else { (m[i - 1] + m[i]) / 2.0 };
    }
    for i in 0..n - 1 {
        if m[i] == 0.0 {
            t[i] = 0.0;
            t[i + 1] = 0.0;
            continue;
        }
        let a = t[i] / m[i];
        let b = t[i + 1] / m[i];
        let s = a * a + b * b;
        if s > 9.0 {
            let k = 3.0 / s.sqrt();
            t[i] = k * a * m[i];
            t[i + 1] = k * b * m[i];
        }
    }
    let mut seg = 0;
    for (i, out) in lut.iter_mut().enumerate() {
        let x = i as f32 / 255.0;
        if x <= pts[0][0] {
            *out = clamp01(pts[0][1]);
            continue;
        }
        if x >= pts[n - 1][0] {
            *out = clamp01(pts[n - 1][1]);
            continue;
        }
        while x > pts[seg + 1][0] {
            seg += 1;
        }
        let h = dx[seg];
        let u = (x - pts[seg][0]) / h;
        let (u2, u3) = (u * u, u * u * u);
        *out = clamp01(
            (2.0 * u3 - 3.0 * u2 + 1.0) * pts[seg][1]
                + (u3 - 2.0 * u2 + u) * h * t[seg]
                + (-2.0 * u3 + 3.0 * u2) * pts[seg + 1][1]
                + (u3 - u2) * h * t[seg + 1],
        );
    }
    lut
}

/// Everything that is a pure per-channel function, folded into one table per
/// channel. Output values are 0..255.
fn tone_luts(st: &LookState) -> [[f32; 256]; 3] {
    let b = &st.basic;
    let warmth = b.warmth / 100.0;
    let tint = b.tint / 100.0;
    let gains = [
        (1.0 + 0.2 * warmth) * (1.0 + 0.05 * tint),
        1.0 - 0.15 * tint,
        (1.0 - 0.2 * warmth) * (1.0 + 0.05 * tint),
    ];
    let gamma = (2.0f32).powf(-b.brightness / 100.0);
    let contrast = (2.0f32).powf(b.contrast / 100.0 * 1.2);
    let sh = b.shadows / 100.0;
    let hi = b.highlights / 100.0;
    let fade = b.fade / 100.0;
    // The asymmetric scales keep the shadow/highlight bumps monotone.
    let sh_k = sh * if sh > 0.0 { 1.5 } else { 0.9 };
    let hi_k = hi * if hi > 0.0 { 0.9 } else { 1.5 };
    let master = curve_lut(&st.curves.rgb);
    let chans = [curve_lut(&st.curves.r), curve_lut(&st.curves.g), curve_lut(&st.curves.b)];

    let mut out = [[0.0f32; 256]; 3];
    for c in 0..3 {
        for i in 0..256 {
            let mut x = clamp01(i as f32 / 255.0 * gains[c]);
            x = x.powf(gamma);
            x = clamp01((x - 0.5) * contrast + 0.5);
            x += sh_k * x * (1.0 - x) * (1.0 - x);
            x += hi_k * x * x * (1.0 - x);
            x = fade * 0.15 + x * (1.0 - fade * 0.25);
            x = master[(clamp01(x) * 255.0).round() as usize];
            out[c][i] = chans[c][(x * 255.0).round() as usize] * 255.0;
        }
    }
    out
}

fn scaled_lut(pts: &[[f32; 2]]) -> [f32; 256] {
    let mut l = curve_lut(pts);
    for v in &mut l {
        *v *= 255.0;
    }
    l
}

/// Horizontal then vertical running-mean box blur on a `w`x`h` plane, repeated
/// `passes` times; the result ends up in `a`. Values truncate to u16 like the
/// original's typed arrays.
fn box_blur(a: &mut [u16], w: usize, h: usize, r: usize, passes: usize) {
    let mut tmp = vec![0u16; a.len()];
    let win = (2 * r + 1) as i64;
    for _ in 0..passes {
        for y in 0..h {
            let row = y * w;
            let mut sum = a[row] as i64 * (r as i64 + 1);
            for k in 1..=r {
                sum += a[row + k.min(w - 1)] as i64;
            }
            for x in 0..w {
                tmp[row + x] = (sum / win) as u16;
                let add = (x + r + 1).min(w - 1);
                let sub = x.saturating_sub(r);
                sum += a[row + add] as i64 - a[row + sub] as i64;
            }
        }
        for x in 0..w {
            let mut sum = tmp[x] as i64 * (r as i64 + 1);
            for k in 1..=r {
                sum += tmp[x + k.min(h - 1) * w] as i64;
            }
            for y in 0..h {
                a[x + y * w] = (sum / win) as u16;
                let add = (y + r + 1).min(h - 1);
                let sub = y.saturating_sub(r);
                sum += tmp[x + add * w] as i64 - tmp[x + sub * w] as i64;
            }
        }
    }
}

fn luma(p: &[u8]) -> f32 {
    0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32
}

fn lens_blur(src: &[u8], w: usize, h: usize, b: &Blur) -> Vec<u8> {
    let n = w * h;
    let rad = ((b.amount / 100.0 * 0.02 * w.max(h) as f32).round() as usize).max(1);
    let inner = b.size / 100.0 * 0.6;
    let outer = inner + 0.02 + b.transition / 100.0 * 0.6;
    let s = w.min(h) as f32;
    let ang = b.angle.to_radians();
    let (sn, cs) = ang.sin_cos();
    let radial = b.shape == "radial";
    let mut mask = vec![0u8; n];
    for y in 0..h {
        let py = (y as f32 + 0.5 - b.cy * h as f32) / s;
        for x in 0..w {
            let px = (x as f32 + 0.5 - b.cx * w as f32) / s;
            let d = if radial { (px * px + py * py).sqrt() } else { (py * cs - px * sn).abs() };
            let t = clamp01((d - inner) / (outer - inner));
            mask[y * w + x] = (t * t * (3.0 - 2.0 * t) * 255.0) as u8;
        }
    }
    // Blurring squared values (roughly linear light) lets highlights bloom the
    // way they do through a real lens. Channels are independent.
    let planes: Vec<Vec<u8>> = thread::scope(|sc| {
        let mask = &mask;
        let handles: Vec<_> = (0..3)
            .map(|c| {
                sc.spawn(move || {
                    let mut a: Vec<u16> =
                        (0..n).map(|p| (src[p * 4 + c] as u16) * (src[p * 4 + c] as u16)).collect();
                    box_blur(&mut a, w, h, rad, 3);
                    (0..n)
                        .map(|p| {
                            let m = mask[p] as f32;
                            let s = src[p * 4 + c] as f32;
                            if m == 0.0 {
                                src[p * 4 + c]
                            } else {
                                (s + ((a[p] as f32).sqrt() - s) * m / 255.0).round().clamp(0.0, 255.0) as u8
                            }
                        })
                        .collect::<Vec<u8>>()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut out = vec![0u8; n * 4];
    for p in 0..n {
        out[p * 4] = planes[0][p];
        out[p * 4 + 1] = planes[1][p];
        out[p * 4 + 2] = planes[2][p];
        out[p * 4 + 3] = src[p * 4 + 3];
    }
    out
}

/// Heavily blurred luminance, used by ambience to tell bright regions from dark.
fn blurred_luma(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut a: Vec<u16> = (0..w * h).map(|p| (luma(&src[p * 4..]) * 256.0) as u16).collect();
    box_blur(&mut a, w, h, ((0.06 * w.max(h) as f32).round() as usize).max(1), 2);
    a.iter().map(|&v| (v >> 8) as u8).collect()
}

/// Luminance minus its blur at radius `r`, scaled by 64: the detail at that scale.
fn detail_map(base: &[u8], w: usize, h: usize, r: usize) -> Vec<i16> {
    let mut a: Vec<u16> = (0..w * h).map(|p| (luma(&base[p * 4..]) * 64.0) as u16).collect();
    let orig = a.clone();
    box_blur(&mut a, w, h, r, 2);
    orig.iter().zip(&a).map(|(&o, &b)| o as i16 - b as i16).collect()
}

fn grain_cell(size: f32, w: usize, h: usize) -> f32 {
    ((0.4 + size / 100.0 * 2.6) * w.max(h) as f32 / 1600.0).max(1.0)
}

fn make_grain(w: usize, h: usize, cell: f32) -> Vec<i8> {
    let mut s: u32 = 0x9e37_79b9;
    let mut rnd = move || {
        s = s.wrapping_add(0x6D2B_79F5);
        let mut t = (s ^ (s >> 15)).wrapping_mul(1 | s);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        (t ^ (t >> 14)) as f32 / 4_294_967_296.0
    };
    let mut g = move || {
        let a = rnd();
        let b = rnd();
        let c = rnd();
        (a + b + c - 1.5) * 84.6
    };
    let mut out = vec![0i8; w * h];
    if cell <= 1.05 {
        for v in &mut out {
            *v = g() as i8;
        }
        return out;
    }
    let gw = (w as f32 / cell).ceil() as usize + 2;
    let gh = (h as f32 / cell).ceil() as usize + 2;
    // Interpolation averages the noise down, so overdrive the grid a little.
    let grid: Vec<f32> = (0..gw * gh).map(|_| g() * 1.4).collect();
    for y in 0..h {
        let fy = y as f32 / cell;
        let y0 = fy as usize;
        let ty = fy - y0 as f32;
        for x in 0..w {
            let fx = x as f32 / cell;
            let x0 = fx as usize;
            let tx = fx - x0 as f32;
            let q = y0 * gw + x0;
            let top = grid[q] + (grid[q + 1] - grid[q]) * tx;
            let bot = grid[q + gw] + (grid[q + gw + 1] - grid[q + gw]) * tx;
            out[y * w + x] = (top + (bot - top) * ty).clamp(-127.0, 127.0) as i8;
        }
    }
    out
}

/// Applies one look to straight-alpha RGBA8 pixels.
pub fn apply_look(src: &[u8], w: usize, h: usize, st: &LookState) -> Vec<u8> {
    let b = &st.basic;
    let preset = PRESETS.iter().find(|p| p.id == st.filter.id);
    let fk = if preset.is_some() { st.filter.strength / 100.0 } else { 0.0 };

    let amb = b.ambience / 100.0;
    let luma_blur = if amb != 0.0 { Some(blurred_luma(src, w, h)) } else { None };
    let amb_k = amb * 0.9 * 2.0 / 255.0;

    let blurred = if st.blur.amount > 0.0 { Some(lens_blur(src, w, h, &st.blur)) } else { None };
    let base: &[u8] = blurred.as_deref().unwrap_or(src);

    let long = w.max(h) as f32;
    let sharp = (st.detail.sharpening > 0.0)
        .then(|| detail_map(base, w, h, ((long / 1600.0).round() as usize).max(1)));
    let strukt = (st.detail.structure != 0.0)
        .then(|| detail_map(base, w, h, ((long * 0.012).round() as usize).max(2)));
    let sharp_k = st.detail.sharpening / 100.0 * 1.5 / 64.0;
    let struct_k = st.detail.structure / 100.0 / 64.0;

    let grain_amt = st.grain.amount / 100.0 + preset.map_or(0.0, |p| p.grain) * fk;
    let noise = (grain_amt > 0.0).then(|| make_grain(w, h, grain_cell(st.grain.size, w, h)));
    let grain_k = grain_amt * 0.6;

    let [lr, lg, lb] = tone_luts(st);
    let sat = ((1.0 + b.saturation / 100.0) * (1.0 + 0.3 * amb)).max(0.0);
    let filt = preset.filter(|_| fk > 0.0).map(|p| {
        ([scaled_lut(p.r), scaled_lut(p.g), scaled_lut(p.b)], p.sat)
    });
    let vig = b.vignette / 100.0 * 0.75 + preset.map_or(0.0, |p| p.vig) * fk;
    let vx2: Vec<f32> = (0..w)
        .map(|x| {
            let v = (x as f32 + 0.5) / w as f32 * 2.0 - 1.0;
            v * v
        })
        .collect();

    let idx = |v: f32| -> usize { (v + 0.5).clamp(0.0, 255.0) as usize };

    let mut out = vec![0u8; w * h * 4];
    let threads = thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    let rows_per = h.div_ceil(threads).max(1);
    thread::scope(|sc| {
        for (band, chunk) in out.chunks_mut(rows_per * w * 4).enumerate() {
            let (lr, lg, lb) = (&lr, &lg, &lb);
            let (sharp, strukt, noise) = (&sharp, &strukt, &noise);
            let (luma_blur, filt, vx2) = (&luma_blur, &filt, &vx2);
            sc.spawn(move || {
                let y_start = band * rows_per;
                for (ry, row) in chunk.chunks_mut(w * 4).enumerate() {
                    let y = y_start + ry;
                    let vy = (y as f32 + 0.5) / h as f32 * 2.0 - 1.0;
                    let vy2 = vy * vy;
                    for x in 0..w {
                        let p = y * w + x;
                        let i = p * 4;
                        let mut r = base[i] as f32;
                        let mut g = base[i + 1] as f32;
                        let mut bl = base[i + 2] as f32;

                        if sharp.is_some() || strukt.is_some() {
                            let d = sharp.as_ref().map_or(0.0, |s| s[p] as f32 * sharp_k)
                                + strukt.as_ref().map_or(0.0, |s| s[p] as f32 * struct_k);
                            r = (r + d).clamp(0.0, 255.0);
                            g = (g + d).clamp(0.0, 255.0);
                            bl = (bl + d).clamp(0.0, 255.0);
                        }

                        if let Some(lm) = luma_blur {
                            // Lift where the neighbourhood is dark, pull down where it is bright.
                            let d = (127.5 - lm[p] as f32) / 255.0 * amb_k;
                            r += d * r * (255.0 - r);
                            g += d * g * (255.0 - g);
                            bl += d * bl * (255.0 - bl);
                        }

                        r = lr[idx(r)];
                        g = lg[idx(g)];
                        bl = lb[idx(bl)];

                        if sat != 1.0 {
                            let l = 0.299 * r + 0.587 * g + 0.114 * bl;
                            r = l + (r - l) * sat;
                            g = l + (g - l) * sat;
                            bl = l + (bl - l) * sat;
                        }

                        if let Some(([fr, fg, fb], fsat)) = filt {
                            let (mut r2, mut g2, mut b2) = (r, g, bl);
                            if *fsat != 1.0 {
                                let l = 0.299 * r + 0.587 * g + 0.114 * bl;
                                r2 = l + (r - l) * fsat;
                                g2 = l + (g - l) * fsat;
                                b2 = l + (bl - l) * fsat;
                            }
                            r2 = fr[idx(r2)];
                            g2 = fg[idx(g2)];
                            b2 = fb[idx(b2)];
                            r += (r2 - r) * fk;
                            g += (g2 - g) * fk;
                            bl += (b2 - bl) * fk;
                        }

                        if vig != 0.0 {
                            let t = clamp01((vx2[x] + vy2 - 0.3) / 1.5);
                            let m = 1.0 - vig * t * t * (3.0 - 2.0 * t);
                            r *= m;
                            g *= m;
                            bl *= m;
                        }

                        if let Some(nz) = noise {
                            // Strongest in the midtones, like film.
                            let l = clamp01((0.299 * r + 0.587 * g + 0.114 * bl) / 255.0);
                            let a = nz[p] as f32 * grain_k * (0.35 + 2.6 * l * (1.0 - l));
                            r += a;
                            g += a;
                            bl += a;
                        }

                        let o = x * 4;
                        row[o] = r.round().clamp(0.0, 255.0) as u8;
                        row[o + 1] = g.round().clamp(0.0, 255.0) as u8;
                        row[o + 2] = bl.round().clamp(0.0, 255.0) as u8;
                        row[o + 3] = base[i + 3];
                    }
                }
            });
        }
    });
    out
}

/// Applies looks in order, each to the previous one's output.
pub fn apply_stack(src: &[u8], w: usize, h: usize, looks: &[Look]) -> Vec<u8> {
    let mut cur = src.to_vec();
    for look in looks {
        cur = apply_look(&cur, w, h, &look.state);
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: usize, h: usize) -> Vec<u8> {
        (0..w * h * 4)
            .map(|i| if i % 4 == 3 { 255 } else { ((i / 4 * 7 + i % 4 * 40) % 256) as u8 })
            .collect()
    }

    #[test]
    fn default_look_is_identity() {
        let src = gradient(37, 23);
        let out = apply_look(&src, 37, 23, &LookState::default());
        assert_eq!(src, out);
    }

    #[test]
    fn parses_export_and_bare_array() {
        let wrapped = r#"{"format":"snaplite-looks","version":1,"looks":[
            {"name":"warm","state":{"basic":{"warmth":40},"filter":{"id":"gold","strength":50}}}]}"#;
        let bare = r#"[{"name":"x","state":{}}]"#;
        let dir = std::env::temp_dir().join("geeq-looks-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.json"), wrapped).unwrap();
        std::fs::write(dir.join("b.json"), bare).unwrap();
        let a = load_looks(&dir.join("a.json")).unwrap();
        assert_eq!(a[0].state.basic.warmth, 40.0);
        assert_eq!(a[0].state.basic.contrast, 0.0);
        assert_eq!(a[0].state.filter.strength, 50.0);
        assert_eq!(a[0].state.curves.rgb, identity());
        assert_eq!(load_looks(&dir.join("b.json")).unwrap()[0].name, "x");
    }

    #[test]
    fn brightness_brightens_and_stacks() {
        let src = gradient(16, 16);
        let mut st = LookState::default();
        st.basic.brightness = 50.0;
        let look = Look { name: "b".into(), state: st };
        let once = apply_stack(&src, 16, 16, &[look.clone()]);
        let twice = apply_stack(&src, 16, 16, &[look.clone(), look]);
        let sum = |v: &[u8]| v.iter().map(|&x| x as u64).sum::<u64>();
        assert!(sum(&once) > sum(&src));
        assert!(sum(&twice) > sum(&once));
    }
}
