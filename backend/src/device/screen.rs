//! Screen capture: the scope's rendered framebuffer (selector `0x20`).
//!
//! This is the screen the instrument is actually drawing — grid, menus, measurements and,
//! notably, the logic-analyzer rows that no waveform read exposes. The scope sends
//! RGB565 little-endian pixels; [`Screenshot`] holds them converted to 8-bit RGB.

/// Framebuffer width in pixels.
pub const SCREEN_WIDTH: usize = 800;
/// Framebuffer height in pixels.
pub const SCREEN_HEIGHT: usize = 480;
/// Exact byte length of one framebuffer transfer (RGB565 = 2 bytes per pixel).
pub const FRAMEBUFFER_BYTES: usize = SCREEN_WIDTH * SCREEN_HEIGHT * 2;

/// A decoded screen grab, stored as tightly packed 8-bit RGB triplets.
#[derive(Debug, Clone)]
pub struct Screenshot {
    width: usize,
    height: usize,
    /// `width * height * 3` bytes, row-major, `R, G, B` per pixel.
    rgb: Vec<u8>,
}

impl Screenshot {
    /// Convert a raw RGB565 little-endian framebuffer into a screenshot.
    ///
    /// Extra trailing bytes are ignored; the input must hold at least a full screen.
    pub fn from_rgb565(raw: &[u8]) -> Option<Self> {
        if raw.len() < FRAMEBUFFER_BYTES {
            return None;
        }
        let mut rgb = Vec::with_capacity(SCREEN_WIDTH * SCREEN_HEIGHT * 3);
        for pixel in raw[..FRAMEBUFFER_BYTES].chunks_exact(2) {
            let value = u16::from_le_bytes([pixel[0], pixel[1]]);
            // RGB565 → RGB888: shift each field up and leave the low bits zero.
            rgb.push(((value >> 11) as u8 & 0x1f) << 3); // red   (5 bits)
            rgb.push(((value >> 5) as u8 & 0x3f) << 2); // green (6 bits)
            rgb.push((value as u8 & 0x1f) << 3); // blue  (5 bits)
        }
        Some(Self {
            width: SCREEN_WIDTH,
            height: SCREEN_HEIGHT,
            rgb,
        })
    }

    /// Image width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Image height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The packed 8-bit RGB pixel data.
    pub fn rgb(&self) -> &[u8] {
        &self.rgb
    }

    /// The `(r, g, b)` value at `(x, y)`, or `None` if out of bounds.
    pub fn pixel(&self, x: usize, y: usize) -> Option<(u8, u8, u8)> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y * self.width + x) * 3;
        Some((self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]))
    }
}
