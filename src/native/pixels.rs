//! Frames guests draw themselves, shown in native windows without a GPU.

use std::num::NonZeroU32;
use std::sync::Arc;

use softbuffer::{Context, Surface};
use winit::event_loop::{ActiveEventLoop, OwnedDisplayHandle};
use winit::window::Window;

use crate::host::log;
use crate::system::{PixelFormat, PixelFrame};

/// What a window that is drawn this way keeps.
pub(super) struct Painted {
    surface: Surface<OwnedDisplayHandle, Arc<Window>>,
    /// The frame on show: a window is asked for its content again whenever
    /// its size changes or the system uncovers it, sooner than its guest
    /// can answer.
    frame: PixelFrame,
}

/// The connection to the display that every window's frames go through.
pub(super) struct Painter {
    context: Context<OwnedDisplayHandle>,
}

impl Painter {
    pub(super) fn new(event_loop: &ActiveEventLoop) -> Option<Self> {
        match Context::new(event_loop.owned_display_handle()) {
            Ok(context) => Some(Self { context }),
            Err(error) => {
                log(|| format!("frames cannot be shown without a GPU here: {error}"));
                None
            }
        }
    }

    /// Shows `frame` in `window`, which is `painted` if it has been drawn
    /// this way before.
    pub(super) fn show(
        &self,
        window: &Arc<Window>,
        painted: &mut Option<Painted>,
        frame: PixelFrame,
    ) {
        match painted {
            Some(painted) => painted.frame = frame,
            None => match Surface::new(&self.context, Arc::clone(window)) {
                Ok(surface) => *painted = Some(Painted { surface, frame }),
                Err(error) => {
                    log(|| format!("a window cannot be drawn on without a GPU: {error}"));
                    return;
                }
            },
        }
        if let Some(painted) = painted {
            painted.show();
        }
    }
}

impl Painted {
    /// Puts the frame on the window, at the size the window has now.
    pub(super) fn show(&mut self) {
        let window = Arc::clone(self.surface.window());
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            // Minimised: there is nothing to draw on.
            return;
        };
        if let Err(error) = self.surface.resize(width, height) {
            log(|| format!("a window's frame could not be resized: {error}"));
            return;
        }
        let mut buffer = match self.surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(error) => {
                log(|| format!("a window's frame could not be drawn: {error}"));
                return;
            }
        };
        fill(&mut buffer, size.width, size.height, &self.frame);
        // Where the window system says when it wants the next frame
        // (Wayland), this is what asks it to.
        window.pre_present_notify();
        if let Err(error) = buffer.present() {
            log(|| format!("a window's frame could not be shown: {error}"));
        }
    }
}

/// Writes `frame` into `out`, `width` by `height` pixels of `0x00RRGGBB`. A
/// frame of another size is stretched: each pixel of the window takes the
/// nearest one of the frame.
fn fill(out: &mut [u32], width: u32, height: u32, frame: &PixelFrame) {
    let (red, blue) = match frame.format {
        PixelFormat::Bgra8 => (2, 0),
        PixelFormat::Rgba8 => (0, 2),
    };
    let pixel = |bytes: &[u8]| {
        u32::from(bytes[red]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[blue])
    };
    let same = (width, height) == (frame.width, frame.height);
    for (y, row) in out.chunks_exact_mut(width as usize).enumerate() {
        if y as u32 >= height {
            break;
        }
        let from = if same {
            y as u32
        } else {
            (u64::from(y as u32) * u64::from(frame.height) / u64::from(height)) as u32
        };
        let source = frame.row(from);
        if same {
            for (out, bytes) in row.iter_mut().zip(source.chunks_exact(4)) {
                *out = pixel(bytes);
            }
        } else {
            for (x, out) in row.iter_mut().enumerate() {
                let from = (x as u64 * u64::from(frame.width) / u64::from(width)) as usize;
                *out = pixel(&source[from * 4..from * 4 + 4]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::PixelAlpha;

    fn frame(format: PixelFormat) -> PixelFrame {
        PixelFrame {
            width: 2,
            height: 2,
            stride: 10,
            format,
            alpha: PixelAlpha::Opaque,
            // Each row: two pixels and two bytes that are not pixels.
            pixels: vec![
                1, 2, 3, 0xFF, 4, 5, 6, 0xFF, 0xEE, 0xEE, //
                7, 8, 9, 0xFF, 10, 11, 12, 0xFF,
            ],
            damage: Vec::new(),
        }
    }

    #[test]
    fn a_frame_fills_the_window_in_the_window_s_own_layout() {
        let mut out = [0; 4];
        fill(&mut out, 2, 2, &frame(PixelFormat::Rgba8));
        assert_eq!(out, [0x010203, 0x040506, 0x070809, 0x0A0B0C]);
        fill(&mut out, 2, 2, &frame(PixelFormat::Bgra8));
        assert_eq!(out, [0x030201, 0x060504, 0x090807, 0x0C0B0A]);
    }

    #[test]
    fn a_frame_of_another_size_is_stretched_to_the_window() {
        let mut out = [0; 16];
        fill(&mut out, 4, 4, &frame(PixelFormat::Rgba8));
        assert_eq!(
            out,
            [
                0x010203, 0x010203, 0x040506, 0x040506, //
                0x010203, 0x010203, 0x040506, 0x040506, //
                0x070809, 0x070809, 0x0A0B0C, 0x0A0B0C, //
                0x070809, 0x070809, 0x0A0B0C, 0x0A0B0C,
            ]
        );
        let mut out = [0; 1];
        fill(&mut out, 1, 1, &frame(PixelFormat::Rgba8));
        assert_eq!(out, [0x010203]);
    }
}
