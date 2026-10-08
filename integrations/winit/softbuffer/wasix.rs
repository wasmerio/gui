//! WASIX (`wasm32-wasmer-wasi`): frames go to the Wasmer runtime, which shows them in the
//! window: natively, in a browser's canvas, or nowhere on a host nobody watches.
//!
//! The pixels live in the program's memory as a `Vec<u32>` in softbuffer's `0x00RRGGBB`, which
//! in WebAssembly's little-endian memory is blue, green, red and a byte that means nothing:
//! `WGUIPixelFormat_Bgra8`, opaque. `wguiWindowPresentPixels` copies the frame out during the
//! call, so the buffer is the program's again at once, as it left it.
//!
//! Presenting does not wait for the display. What paces a program is its window's redraws:
//! the host answers a request for one when the display is ready for the next frame.
//!
//! raw-window-handle has no handles for WASIX. The web's are borrowed: the window is
//! `RawWindowHandle::Web(WebWindowHandle { id })`, whose `id` is the host's handle of the
//! window (a `WGUIWindow`).

use crate::backend_interface::*;
use crate::error::InitError;
use crate::{util, Rect, SoftBufferError};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawWindowHandle};
use std::marker::PhantomData;
use std::num::NonZeroU32;
use wasmer_gui_sys::{
    wguiWindowPresentPixels, WGUIPixelAlpha_Opaque, WGUIPixelFormat_Bgra8, WGUIPixelFrame,
    WGUIRect, WGUIStatus_Success,
};

#[derive(Debug)]
pub struct WasixImpl<D, W> {
    /// The host's handle of the window.
    window_id: u32,

    /// The buffer that we're drawing to.
    buffer: util::PixelBuffer,

    /// Buffer has been presented.
    buffer_presented: bool,

    /// The current buffer width/height.
    size: Option<(NonZeroU32, NonZeroU32)>,

    /// The underlying window handle.
    window_handle: W,

    /// The underlying display handle.
    _display: PhantomData<D>,
}

impl<D: HasDisplayHandle, W: HasWindowHandle> WasixImpl<D, W> {
    fn present_with_damage(&mut self, damage: &[Rect]) -> Result<(), SoftBufferError> {
        let (width, height) = self
            .size
            .expect("Must set size of surface before calling `present_with_damage()`");

        for rect in damage {
            let in_range = |start: u32, len: NonZeroU32, max: NonZeroU32| {
                start
                    .checked_add(len.get())
                    .is_some_and(|end| end <= max.get())
            };
            if !in_range(rect.x, rect.width, width) || !in_range(rect.y, rect.height, height) {
                return Err(SoftBufferError::DamageOutOfRange { rect: *rect });
            }
        }

        let frame = WGUIPixelFrame {
            width: width.get(),
            height: height.get(),
            format: WGUIPixelFormat_Bgra8,
            alpha: WGUIPixelAlpha_Opaque,
            pixels: self.buffer.as_ptr().cast(),
            pixelsSize: self.buffer.len() * 4,
            ..WGUIPixelFrame::INIT
        };
        // No rectangles means all of the frame changed.
        let whole = damage.iter().any(|rect| {
            rect.x == 0 && rect.y == 0 && rect.width == width && rect.height == height
        });
        let rects: Vec<WGUIRect> = if whole {
            Vec::new()
        } else {
            damage
                .iter()
                .map(|rect| WGUIRect {
                    x: rect.x as i32,
                    y: rect.y as i32,
                    width: rect.width.get(),
                    height: rect.height.get(),
                })
                .collect()
        };
        if !whole && rects.is_empty() {
            // Nothing changed: there is nothing to show.
            return Ok(());
        }

        let status =
            unsafe { wguiWindowPresentPixels(self.window_id, &frame, rects.len(), rects.as_ptr()) };
        if status != WGUIStatus_Success {
            return Err(SoftBufferError::PlatformError(
                Some(format!(
                    "the host would not show the frame (wguiWindowPresentPixels: status {status})"
                )),
                None,
            ));
        }
        self.buffer_presented = true;
        Ok(())
    }
}

impl<D: HasDisplayHandle, W: HasWindowHandle> SurfaceInterface<D, W> for WasixImpl<D, W> {
    /// There is a single implicit host display: the context is just the display handle
    /// (through the blanket `impl ContextInterface<D> for D`).
    type Context = D;
    type Buffer<'a>
        = BufferImpl<'a, D, W>
    where
        Self: 'a;

    fn new(window: W, _display: &D) -> Result<Self, InitError<W>> {
        let raw = window.window_handle()?.as_raw();
        let RawWindowHandle::Web(handle) = raw else {
            return Err(InitError::Unsupported(window));
        };

        Ok(Self {
            window_id: handle.id,
            buffer: util::PixelBuffer(Vec::new()),
            buffer_presented: false,
            size: None,
            window_handle: window,
            _display: PhantomData,
        })
    }

    #[inline]
    fn window(&self) -> &W {
        &self.window_handle
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), SoftBufferError> {
        if self.size != Some((width, height)) {
            let len = usize::try_from(width.get())
                .ok()
                .zip(usize::try_from(height.get()).ok())
                .and_then(|(w, h)| w.checked_mul(h))
                // The frame must be addressable as bytes in 32-bit linear memory.
                .filter(|len| len.checked_mul(4).is_some())
                .ok_or(SoftBufferError::SizeOutOfRange { width, height })?;

            self.buffer_presented = false;
            self.buffer.resize(len, 0);
            self.size = Some((width, height));
        }

        Ok(())
    }

    fn buffer_mut(&mut self) -> Result<BufferImpl<'_, D, W>, SoftBufferError> {
        if self.size.is_none() {
            panic!("Must set size of surface before calling `buffer_mut()`");
        }

        Ok(BufferImpl { imp: self })
    }

    // `fetch()` keeps the default: `Err(SoftBufferError::Unimplemented)`.
}

#[derive(Debug)]
pub struct BufferImpl<'a, D, W> {
    imp: &'a mut WasixImpl<D, W>,
}

impl<D: HasDisplayHandle, W: HasWindowHandle> BufferInterface for BufferImpl<'_, D, W> {
    fn width(&self) -> NonZeroU32 {
        self.imp
            .size
            .expect("must set size of surface before calling `width()` on the buffer")
            .0
    }

    fn height(&self) -> NonZeroU32 {
        self.imp
            .size
            .expect("must set size of surface before calling `height()` on the buffer")
            .1
    }

    #[inline]
    fn pixels(&self) -> &[u32] {
        &self.imp.buffer
    }

    #[inline]
    fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.imp.buffer
    }

    /// The buffer is retained between frames: `1` once it has been presented, `0` after a resize.
    fn age(&self) -> u8 {
        if self.imp.buffer_presented {
            1
        } else {
            0
        }
    }

    fn present(self) -> Result<(), SoftBufferError> {
        let (width, height) = self
            .imp
            .size
            .expect("Must set size of surface before calling `present()`");
        self.imp.present_with_damage(&[Rect {
            x: 0,
            y: 0,
            width,
            height,
        }])
    }

    fn present_with_damage(self, damage: &[Rect]) -> Result<(), SoftBufferError> {
        self.imp.present_with_damage(damage)
    }
}
