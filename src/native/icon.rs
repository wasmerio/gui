//! The guest's icon on the desktop (`WindowChange::Icon`).
//!
//! - macOS has one icon for the whole process, in the Dock and the app
//!   switcher: the one given last to a window that is still open becomes
//!   the application's ([`AppIcon`], which the loop keeps). With no such
//!   window, and once the loop has ended however it ended, the icon the
//!   application had before is back. It is given every image, as the
//!   representations of one picture, and draws the one that suits each
//!   place.
//! - Windows has two for each window: a small one for the title bar, and a
//!   large one for the task bar and the window switcher. Each is one image,
//!   the one for the size Windows draws it at on the window's display.
//! - X11 has one for each window, which task bars and window switchers draw
//!   smaller. Wayland has none that winit sets: the feature is not offered
//!   there.

use std::sync::Arc;

use winit::window::Window;

use crate::system::Icon;

/// The image X11 is given: big enough for a window switcher, small enough
/// for a window property.
#[cfg(all(unix, not(target_os = "macos")))]
const X11_EDGE: u32 = 128;

/// Whether the process has one icon, the application's, rather than one for
/// each window.
pub(super) const ONE_FOR_THE_PROCESS: bool = cfg!(target_os = "macos");

/// Shows `icon` for `window`, or the system's own icon with `None`, where
/// windows have icons of their own.
pub(super) fn show(window: &Window, icon: Option<&Icon>) {
    #[cfg(target_os = "macos")]
    let _ = (window, icon);
    #[cfg(target_os = "windows")]
    {
        use winit::platform::windows::WindowExtWindows;
        // What Windows draws them at on a display of scale factor 1.
        let edge = |at_one: f64| (at_one * window.scale_factor()).round() as u32;
        window.set_window_icon(icon.and_then(|icon| winit_icon(icon, edge(16.0))));
        window.set_taskbar_icon(icon.and_then(|icon| winit_icon(icon, edge(32.0))));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    window.set_window_icon(icon.and_then(|icon| winit_icon(icon, X11_EDGE)));
}

/// The application's icon, where the process has one
/// ([`ONE_FOR_THE_PROCESS`]): a guest's while a window has it, and otherwise
/// the one the application had before. That one comes back when this is
/// dropped, so it does however the loop ends, even with windows a guest
/// never closed. On the main thread.
pub(super) struct AppIcon {
    /// The guest's icon on show.
    shown: Option<Arc<Icon>>,
    /// The application's icon from before the guest's, while that is shown.
    #[cfg(target_os = "macos")]
    before: Option<mac::Before>,
}

impl AppIcon {
    pub(super) fn new() -> Self {
        Self {
            shown: None,
            #[cfg(target_os = "macos")]
            before: None,
        }
    }

    /// Shows `icon` as the application's, or with `None` the one it had
    /// before.
    pub(super) fn show(&mut self, icon: Option<Arc<Icon>>) {
        if crate::host::same_icon(&icon, &self.shown) {
            return;
        }
        #[cfg(target_os = "macos")]
        mac::show(&mut self.before, icon.as_deref());
        self.shown = icon;
    }
}

impl Drop for AppIcon {
    fn drop(&mut self) {
        self.show(None);
    }
}

/// Whether the icon is drawn at sizes that follow the window's display, and
/// is to be shown again when the window moves to a display of another scale.
pub(super) const FOLLOWS_SCALE: bool = cfg!(target_os = "windows");

#[cfg(not(target_os = "macos"))]
fn winit_icon(icon: &Icon, edge: u32) -> Option<winit::window::Icon> {
    let image = icon.image_for(edge)?;
    winit::window::Icon::from_rgba(image.rgba.clone(), image.width, image.height)
        .map_err(|error| crate::host::log(|| format!("an icon winit does not take: {error}")))
        .ok()
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::rc::Retained;
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{
        NSApplication, NSBitmapFormat, NSBitmapImageRep, NSColorSpace, NSDeviceRGBColorSpace,
        NSImage,
    };
    use objc2_foundation::{NSCopying, NSSize};

    use crate::system::{Icon, IconImage};

    /// A copy of the application's icon from before the guest's. A copy,
    /// because AppKit changes the picture it hands out into the one it is
    /// given next.
    pub(super) struct Before(Option<Retained<NSImage>>);

    /// Shows `icon` as the application's, keeping the one from before in
    /// `before` if it is not kept yet; with `None`, or an icon AppKit takes
    /// none of the images of, puts that one back.
    pub(super) fn show(before: &mut Option<Before>, icon: Option<&Icon>) {
        let Some(main) = MainThreadMarker::new() else {
            return;
        };
        let application = NSApplication::sharedApplication(main);
        let image = match icon.and_then(ns_image) {
            Some(image) => {
                before.get_or_insert_with(|| {
                    Before(application.applicationIconImage().map(|image| image.copy()))
                });
                Some(image)
            }
            None => match before.take() {
                Some(Before(image)) => image,
                // The guest's was never shown.
                None => return,
            },
        };
        // SAFETY: on the main thread. nil is taken: it puts the icon of the
        // application's bundle back, as AppKit documents.
        unsafe { application.setApplicationIconImage(image.as_deref()) };
    }

    /// One picture with every image as a representation of it: AppKit draws
    /// the one whose pixels suit the size it is drawn at. `None` when AppKit
    /// takes none of them.
    fn ns_image(icon: &Icon) -> Option<Retained<NSImage>> {
        let largest = icon.images.first()?;
        let size = NSSize::new(f64::from(largest.width), f64::from(largest.height));
        let image = NSImage::initWithSize(NSImage::alloc(), size);
        let mut any = false;
        for each in &icon.images {
            let Some(bitmap) = bitmap(each) else {
                crate::host::log(|| "an icon image AppKit does not take".to_owned());
                continue;
            };
            bitmap.setSize(size);
            image.addRepresentation(&bitmap);
            any = true;
        }
        any.then_some(image)
    }

    /// The image as AppKit's bitmap: straight alpha, sRGB.
    fn bitmap(image: &IconImage) -> Option<Retained<NSBitmapImageRep>> {
        let width = isize::try_from(image.width).ok()?;
        let height = isize::try_from(image.height).ok()?;
        // SAFETY: without planes of the caller's, the bitmap allocates its
        // own pixels, laid out as asked: rows of `width * 4` bytes.
        let bitmap = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                width,
                height,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                NSBitmapFormat::AlphaNonpremultiplied,
                width * 4,
                32,
            )
        }?;
        let pixels = bitmap.bitmapData();
        let room = bitmap
            .bytesPerRow()
            .checked_mul(height)
            .and_then(|bytes| usize::try_from(bytes).ok());
        if pixels.is_null() || bitmap.bytesPerRow() != width * 4 || room != Some(image.rgba.len()) {
            return None;
        }
        // SAFETY: the bitmap's pixels are `bytesPerRow * height` bytes, just
        // checked to be as many as the image has.
        unsafe { std::ptr::copy_nonoverlapping(image.rgba.as_ptr(), pixels, image.rgba.len()) };
        // The pixels are sRGB, as every colour of this API is.
        bitmap.bitmapImageRepByRetaggingWithColorSpace(&NSColorSpace::sRGBColorSpace())
    }
}
