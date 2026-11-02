// SPDX-License-Identifier: GPL-3.0-only
//! What a decoder hands the window (architecture §5.4): either BGRA (the tile codec) or the
//! video decoder's own planes, untouched. Colour conversion and scaling happen on the GPU at
//! presentation; nothing on the viewer's CPU rewrites pixels. The CPU conversion here exists for
//! tests and for the no-GPU fallback.

use crate::frame::Frame;

/// How the planes are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Planar {
    /// Three planes: Y full size, U and V at half width and half height (software decoders).
    I420,
    /// Two planes: Y, then interleaved UV at half width and half height (hardware decoders).
    Nv12,
}

/// The YUV ↔ RGB matrix the stream was encoded with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matrix {
    Bt601,
    Bt709,
}

/// Decoded YUV 4:2:0, 8 bits per sample.
#[derive(Clone, PartialEq, Eq)]
pub struct YuvPicture {
    pub width: u32,
    pub height: u32,
    pub layout: Planar,
    /// Plane bytes, each with its own stride (row pitch in bytes).
    pub planes: Vec<Vec<u8>>,
    pub strides: Vec<usize>,
    pub matrix: Matrix,
    /// Full range (0–255) rather than limited (16–235 / 16–240).
    pub full_range: bool,
}

/// A decoded picture, whatever the decoder produced.
#[derive(Clone, PartialEq, Eq)]
pub enum Picture {
    Bgra(Frame),
    Yuv(YuvPicture),
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Picture::Bgra(frame) => write!(f, "Picture::Bgra({}x{})", frame.width, frame.height),
            Picture::Yuv(p) => write!(f, "Picture::Yuv({:?} {}x{})", p.layout, p.width, p.height),
        }
    }
}

impl Picture {
    pub fn width(&self) -> u32 {
        match self {
            Picture::Bgra(f) => f.width,
            Picture::Yuv(p) => p.width,
        }
    }

    pub fn height(&self) -> u32 {
        match self {
            Picture::Bgra(f) => f.height,
            Picture::Yuv(p) => p.height,
        }
    }

    /// BGRA on the CPU: for tests, and for a machine with no usable GPU.
    pub fn to_bgra(&self) -> Frame {
        match self {
            Picture::Bgra(f) => f.clone(),
            Picture::Yuv(p) => p.to_bgra(),
        }
    }
}

impl YuvPicture {
    /// Luma and chroma samples at a pixel (chroma from the 2×2 block the pixel belongs to).
    pub fn sample(&self, x: u32, y: u32) -> (u8, u8, u8) {
        let (cx, cy) = ((x / 2) as usize, (y / 2) as usize);
        let luma = self.planes[0][y as usize * self.strides[0] + x as usize];
        match self.layout {
            Planar::I420 => (
                luma,
                self.planes[1][cy * self.strides[1] + cx],
                self.planes[2][cy * self.strides[2] + cx],
            ),
            Planar::Nv12 => {
                let o = cy * self.strides[1] + cx * 2;
                (luma, self.planes[1][o], self.planes[1][o + 1])
            }
        }
    }

    /// Integer fixed-point conversion (16 fractional bits), nearest chroma, no upsampling
    /// filter: the CPU fallback has to keep up with 1080p60, so no floating point per pixel.
    pub fn to_bgra(&self) -> Frame {
        let mut out = Frame::new(self.width, self.height);
        let (kr, kb) = match self.matrix {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
        };
        let kg = 1.0 - kr - kb;
        let (y_off, y_scale, c_scale) = if self.full_range {
            (0, 1.0, 1.0)
        } else {
            (16, 255.0 / 219.0, 255.0 / 224.0)
        };
        let fix = |v: f64| (v * 65536.0).round() as i32;
        let cy = fix(y_scale);
        let crv = fix(2.0 * (1.0 - kr) * c_scale);
        let cbu = fix(2.0 * (1.0 - kb) * c_scale);
        let cgu = fix(2.0 * kb * (1.0 - kb) / kg * c_scale);
        let cgv = fix(2.0 * kr * (1.0 - kr) / kg * c_scale);
        let clamp = |v: i32| ((v + 32768) >> 16).clamp(0, 255) as u8;
        let (ys, ystride) = (&self.planes[0], self.strides[0]);
        for y in 0..self.height as usize {
            let luma = &ys[y * ystride..y * ystride + self.width as usize];
            let cy_row = y / 2;
            let row = out.row_slice_mut(0, y as u32, self.width);
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let cx = x / 2;
                let (u, v) = match self.layout {
                    Planar::I420 => (
                        self.planes[1][cy_row * self.strides[1] + cx],
                        self.planes[2][cy_row * self.strides[2] + cx],
                    ),
                    Planar::Nv12 => {
                        let o = cy_row * self.strides[1] + cx * 2;
                        (self.planes[1][o], self.planes[1][o + 1])
                    }
                };
                let yy = (i32::from(luma[x]) - y_off) * cy;
                let u = i32::from(u) - 128;
                let v = i32::from(v) - 128;
                px[0] = clamp(yy + cbu * u);
                px[1] = clamp(yy - cgu * u - cgv * v);
                px[2] = clamp(yy + crv * v);
                px[3] = 255;
            }
        }
        out
    }

    /// The reverse conversion, for tests: BGRA → I420 with the given matrix and range.
    pub fn from_bgra(
        frame: &Frame,
        layout: Planar,
        matrix: Matrix,
        full_range: bool,
    ) -> YuvPicture {
        let (w, h) = (frame.width, frame.height);
        let (kr, kb) = match matrix {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
        };
        let kg = 1.0 - kr - kb;
        let (y_off, y_scale, c_scale) = if full_range {
            (0.0, 1.0, 1.0)
        } else {
            (16.0, 219.0 / 255.0, 224.0 / 255.0)
        };
        let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
        let mut yp = vec![0u8; (w * h) as usize];
        let mut up = vec![0u8; cw * ch];
        let mut vp = vec![0u8; cw * ch];
        // Chroma from the top-left pixel of each 2×2 block (good enough for flat test patches).
        for y in 0..h {
            for x in 0..w {
                let p = frame.pixel(x, y);
                let (r, g, b) = (f64::from(p.r), f64::from(p.g), f64::from(p.b));
                let yy = kr * r + kg * g + kb * b;
                yp[(y * w + x) as usize] = (yy * y_scale + y_off).round().clamp(0.0, 255.0) as u8;
                if x % 2 == 0 && y % 2 == 0 {
                    let u = (b - yy) / (2.0 * (1.0 - kb));
                    let v = (r - yy) / (2.0 * (1.0 - kr));
                    let i = (y / 2) as usize * cw + (x / 2) as usize;
                    up[i] = (u * c_scale + 128.0).round().clamp(0.0, 255.0) as u8;
                    vp[i] = (v * c_scale + 128.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        let (planes, strides) = match layout {
            Planar::I420 => (vec![yp, up, vp], vec![w as usize, cw, cw]),
            Planar::Nv12 => {
                let uv: Vec<u8> = up.iter().zip(&vp).flat_map(|(u, v)| [*u, *v]).collect();
                (vec![yp, uv], vec![w as usize, cw * 2])
            }
        };
        YuvPicture {
            width: w,
            height: h,
            layout,
            planes,
            strides,
            matrix,
            full_range,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Bgra;
    use crate::pattern::{PATCHES, TestPattern};

    fn distance(a: Bgra, b: Bgra) -> f64 {
        let d = |x: u8, y: u8| (f64::from(x) - f64::from(y)).powi(2);
        (d(a.r, b.r) + d(a.g, b.g) + d(a.b, b.b)).sqrt()
    }

    #[test]
    fn yuv_round_trips_the_pattern_in_both_layouts_and_matrices() {
        let mut pattern = TestPattern::new(256, 128);
        let frame = pattern.next_frame();
        for layout in [Planar::I420, Planar::Nv12] {
            for matrix in [Matrix::Bt601, Matrix::Bt709] {
                for full in [false, true] {
                    let yuv = YuvPicture::from_bgra(&frame, layout, matrix, full);
                    let back = Picture::Yuv(yuv).to_bgra();
                    for (i, (name, colour)) in PATCHES.iter().enumerate() {
                        let (x, y) = pattern.patch_centre(i);
                        let got = back.pixel(x, y);
                        assert!(
                            distance(got, *colour) < 4.0,
                            "{name} via {layout:?} {matrix:?} full={full}: {got:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn matrices_differ_so_a_mismatch_would_show() {
        let mut pattern = TestPattern::new(64, 64);
        let frame = pattern.next_frame();
        let mut as_709 = YuvPicture::from_bgra(&frame, Planar::I420, Matrix::Bt709, false);
        as_709.matrix = Matrix::Bt601; // decode with the wrong matrix
        let wrong = as_709.to_bgra();
        let (x, y) = pattern.patch_centre(0); // red
        assert!(
            distance(wrong.pixel(x, y), PATCHES[0].1) > 10.0,
            "red should drift"
        );
    }
}
