//! The guest's icon on the desktop (`WindowChange::Icon`).
//!
//! - macOS has one icon for the whole process, in the Dock and the app
//!   switcher: the one given last to a window that is still open becomes
//!   the application's ([`show_app`], which the loop calls), and with no
//!   such window the application's own is back. It is given every image, as
//!   the representations of one picture, and draws the one that suits each
//!   place.
//! - Windows has two for each window: a small one for the title bar, and a
//!   large one for the task bar and the window switcher. Each is one image,
//!   the one for the size Windows draws it at on the window's display.
//! - X11 has one for each window, which task bars and window switchers draw
//!   smaller. Wayland has none that winit sets: the feature is not offered
//!   there.

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

/// Shows `icon` as the application's, or the application's own with `None`,
/// where the process has one icon ([`ONE_FOR_THE_PROCESS`]).
pub(super) fn show_app(icon: Option<&Icon>) {
    #[cfg(target_os = "macos")]
    mac::show(icon);
    #[cfg(not(target_os = "macos"))]
    let _ = icon;
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
    use objc2_foundation::NSSize;

    use crate::system::{Icon, IconImage};

    pub(super) fn show(icon: Option<&Icon>) {
        let Some(main) = MainThreadMarker::new() else {
            return;
        };
        let image = icon.and_then(ns_image);
        // SAFETY: on the main thread; nil puts the application's own icon
        // back, as AppKit documents.
        unsafe {
            NSApplication::sharedApplication(main).setApplicationIconImage(image.as_deref());
        }
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
