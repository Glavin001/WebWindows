//! Viewports that extend past the render target.
//!
//! Direct3D 9 accepts any viewport and clips to it; WebGPU rejects a viewport that is not inside the render target.
//! So the core sets the part of the viewport inside the target and the vertex shader remaps clip space so every
//! vertex still lands on the pixel the original viewport would have put it on:
//!
//! ```text
//! pos.xy = pos.xy * scale + offset * pos.w
//! ```
//!
//! In normalized device coordinates that is `ndc' = ndc * scale + offset`. With the original viewport `(x, w)` and
//! the clamped one `(x', w')`, the original maps `px = x + (ndc + 1) / 2 * w` and the clamped one
//! `px = x' + (ndc' + 1) / 2 * w'`, so `scale = w / w'` and `offset = (2 (x - x') + w) / w' - 1`. Y points down in
//! pixels and up in NDC (`py = y + (1 - ndc) / 2 * h`), giving `scale = h / h'` and
//! `offset = 1 - (2 (y - y') + h) / h'`.
//!
//! Clipping stays right: geometry outside the original viewport was clipped by Direct3D and is outside the clamped
//! one too, now at |ndc'| > 1. Depth range is untouched. Anything else that reads the viewport size (point sprite
//! expansion, the half-pixel offset) must keep using the original.

/// A viewport clamped to the render target and the clip-space fixup that preserves the original mapping.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewportFit {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub scale: [f32; 2],
    pub offset: [f32; 2],
}

impl ViewportFit {
    /// No clamping happened: the fixup is the identity and can be skipped.
    pub fn is_identity(&self) -> bool {
        self.scale == [1.0, 1.0] && self.offset == [0.0, 0.0]
    }

    /// Applies the fixup to a clip-space position, as the vertex shader does.
    pub fn apply(&self, pos: [f32; 4]) -> [f32; 4] {
        [
            pos[0] * self.scale[0] + self.offset[0] * pos[3],
            pos[1] * self.scale[1] + self.offset[1] * pos[3],
            pos[2],
            pos[3],
        ]
    }
}

/// Fits the viewport `(x, y, w, h)` into a `target_w` x `target_h` render target. `None` when the two do not
/// overlap (or the viewport is empty): nothing could be drawn, so the core skips the draw.
pub fn fit_viewport(x: i64, y: i64, w: u32, h: u32, target_w: u32, target_h: u32) -> Option<ViewportFit> {
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w as i64).min(target_w as i64);
    let y1 = (y + h as i64).min(target_h as i64);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (nw, nh) = ((x1 - x0) as f64, (y1 - y0) as f64);
    let (w, h) = (w as f64, h as f64);
    let scale = [(w / nw) as f32, (h / nh) as f32];
    let offset = [((2.0 * (x - x0) as f64 + w) / nw - 1.0) as f32, (1.0 - (2.0 * (y - y0) as f64 + h) / nh) as f32];
    Some(ViewportFit { x: x0 as f32, y: y0 as f32, width: nw as f32, height: nh as f32, scale, offset })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pixel position of an NDC point under a viewport.
    fn to_pixels(ndc: [f32; 2], x: f32, y: f32, w: f32, h: f32) -> [f32; 2] {
        [x + (ndc[0] + 1.0) / 2.0 * w, y + (1.0 - ndc[1]) / 2.0 * h]
    }

    /// Checks that sample points map to the same pixels through the original viewport and through the fit.
    fn check(x: i64, y: i64, w: u32, h: u32, tw: u32, th: u32) -> ViewportFit {
        let fit = fit_viewport(x, y, w, h, tw, th).unwrap();
        assert!(fit.x >= 0.0 && fit.y >= 0.0);
        assert!(fit.x + fit.width <= tw as f32 && fit.y + fit.height <= th as f32);
        for ndc in [[-1.0, -1.0], [1.0, 1.0], [0.0, 0.0], [0.25, -0.75], [-0.6, 0.9]] {
            for clip_w in [1.0f32, 2.5] {
                let want = to_pixels(ndc, x as f32, y as f32, w as f32, h as f32);
                let p = fit.apply([ndc[0] * clip_w, ndc[1] * clip_w, 0.5, clip_w]);
                let got = to_pixels([p[0] / p[3], p[1] / p[3]], fit.x, fit.y, fit.width, fit.height);
                assert!(
                    (got[0] - want[0]).abs() < 1e-3 && (got[1] - want[1]).abs() < 1e-3,
                    "{ndc:?}: {got:?} vs {want:?}"
                );
            }
        }
        fit
    }

    #[test]
    fn inside_is_identity() {
        let f = check(10, 20, 100, 50, 640, 480);
        assert!(f.is_identity());
        assert_eq!((f.x, f.y, f.width, f.height), (10.0, 20.0, 100.0, 50.0));
        assert!(check(0, 0, 640, 480, 640, 480).is_identity());
    }

    #[test]
    fn clamped_on_each_side() {
        let f = check(-100, 0, 400, 300, 640, 480);
        assert_eq!((f.x, f.width), (0.0, 300.0));
        assert!(!f.is_identity());
        let f = check(500, 400, 400, 300, 640, 480);
        assert_eq!((f.x, f.y, f.width, f.height), (500.0, 400.0, 140.0, 80.0));
        check(-50, -70, 800, 600, 640, 480);
        check(0, -1, 640, 482, 640, 480);
        // A viewport larger than the target on all sides.
        let f = check(-1000, -1000, 4000, 4000, 256, 256);
        assert_eq!((f.x, f.y, f.width, f.height), (0.0, 0.0, 256.0, 256.0));
    }

    #[test]
    fn outside_or_empty_is_none() {
        assert_eq!(fit_viewport(640, 0, 100, 100, 640, 480), None);
        assert_eq!(fit_viewport(-100, 0, 100, 100, 640, 480), None);
        assert_eq!(fit_viewport(0, 480, 10, 10, 640, 480), None);
        assert_eq!(fit_viewport(0, 0, 0, 10, 640, 480), None);
        assert_eq!(fit_viewport(0, 0, 10, 10, 0, 0), None);
    }
}
