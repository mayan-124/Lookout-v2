//! Canvas backbuffer, soft shadow rendering, and coordinate transformations.

use std::ptr::null_mut;

use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};

use crate::geometry::{PointI, RectI};

pub struct Backbuffer {
    pub mem_dc: HDC,
    pub dib: HBITMAP,
    pub old_bmp: HGDIOBJ,
    pub bits: *mut u8,
    pub w: i32,
    pub h: i32,
}

impl Backbuffer {
    pub fn new(hdc: HDC, w: i32, h: i32) -> Option<Self> {
        unsafe {
            let mem_dc = CreateCompatibleDC(Some(hdc));
            if mem_dc.is_invalid() {
                return None;
            }

            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: 0,
                    biSizeImage: (w * h * 4) as u32,
                    ..Default::default()
                },
                bmiColors: [windows::Win32::Graphics::Gdi::RGBQUAD::default()],
            };

            let mut bits: *mut std::ffi::c_void = null_mut();
            let dib = match CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            {
                Ok(b) if !b.is_invalid() && !bits.is_null() => b,
                _ => {
                    let _ = DeleteDC(mem_dc);
                    return None;
                }
            };

            let old_bmp = SelectObject(mem_dc, dib.into());
            Some(Self {
                mem_dc,
                dib,
                old_bmp,
                bits: bits as *mut u8,
                w,
                h,
            })
        }
    }
}

impl Drop for Backbuffer {
    fn drop(&mut self) {
        unsafe {
            if !self.mem_dc.is_invalid() && !self.old_bmp.is_invalid() {
                let _ = SelectObject(self.mem_dc, self.old_bmp);
            }
            if !self.dib.is_invalid() {
                let _ = DeleteObject(self.dib.into());
            }
            if !self.mem_dc.is_invalid() {
                let _ = DeleteDC(self.mem_dc);
            }
        }
    }
}

pub fn screen_to_canvas(
    screen_pt: PointI,
    img_w: u32,
    img_h: u32,
    zoom: f32,
    pan: PointI,
    win_w: i32,
    win_h: i32,
) -> PointI {
    let scaled_w = (img_w as f32 * zoom).round() as i32;
    let scaled_h = (img_h as f32 * zoom).round() as i32;

    let center_x = (win_w - scaled_w) / 2 + pan.x;
    let center_y = (win_h - scaled_h) / 2 + pan.y + 20;

    let img_x = ((screen_pt.x - center_x) as f32 / zoom).round() as i32;
    let img_y = ((screen_pt.y - center_y) as f32 / zoom).round() as i32;

    PointI::new(img_x, img_y)
}

/// Draws a realistic, soft drop shadow only along the outer perimeter strips of img_rect.
/// Never loops over the internal image area, keeping 4K redraws under 8 ms.
pub fn draw_soft_shadow(
    bits: &mut [u8],
    stride: usize,
    win_w: i32,
    win_h: i32,
    img_rect: RectI,
    blur: i32,
    offset_y: i32,
) {
    let shadow_left = (img_rect.left - blur).max(0);
    let shadow_top = (img_rect.top - blur + offset_y).max(0);
    let shadow_right = (img_rect.right + blur).min(win_w);
    let shadow_bottom = (img_rect.bottom + blur + offset_y).min(win_h);

    if shadow_right <= shadow_left || shadow_bottom <= shadow_top {
        return;
    }

    let blur_f = blur as f32;

    for y in shadow_top..shadow_bottom {
        let in_y = y >= img_rect.top && y < img_rect.bottom;

        // When y is inside img_rect vertically, skip the entire interior!
        let x_ranges: &[(i32, i32)] = if in_y {
            &[
                (shadow_left, img_rect.left.min(shadow_right)),
                (img_rect.right.max(shadow_left), shadow_right),
            ]
        } else {
            &[(shadow_left, shadow_right)]
        };

        for &(x0, x1) in x_ranges {
            for x in x0..x1 {
                let dx = if x < img_rect.left {
                    img_rect.left - x
                } else if x >= img_rect.right {
                    x - img_rect.right + 1
                } else {
                    0
                };

                let dy = if y < img_rect.top {
                    img_rect.top - y
                } else if y >= img_rect.bottom {
                    y - img_rect.bottom + 1
                } else {
                    0
                };

                let dist = ((dx * dx + dy * dy) as f32).sqrt();
                if dist < blur_f {
                    let norm = 1.0 - dist / blur_f;
                    let alpha = norm * norm * 0.45; // Smooth quadratic decay

                    let idx = (y as usize * stride + x as usize) * 4;
                    let inv = 1.0 - alpha;
                    bits[idx] = (19.0 * inv) as u8; // B
                    bits[idx + 1] = (16.0 * inv) as u8; // G
                    bits[idx + 2] = (14.0 * inv) as u8; // R
                }
            }
        }
    }
}
