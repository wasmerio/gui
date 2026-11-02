//! Title bars the guest draws: where the system's window controls go, what
//! of the title bar is left for the guest, and which presses move or resize
//! the window instead of reaching it.
//!
//! - macOS has both styles. The content view takes the whole window (a
//!   transparent title bar over a full-size content view), and with
//!   `Overlay` the traffic lights are moved to the middle of the title bar
//!   the guest draws. AppKit puts them back whenever it lays the title bar
//!   out (a resize, leaving fullscreen), so they are moved again then.
//! - Windows and Linux have `Custom` only: a window without decorations,
//!   which the host moves (drag regions) and resizes (a band along its
//!   edges) for the guest. Their own controls cannot be kept over content
//!   drawn with a GPU: Windows draws its caption buttons under the client
//!   area, and X11 and Wayland draw none inside it.

use winit::window::ResizeDirection;

use crate::system::{Rect, TitleBarStyle};

/// How wide the band along a `Custom` window's edges is that resizes it, in
/// logical units.
pub(super) const RESIZE_BAND: f64 = 5.0;

/// Whether the host resizes a `style` window from a band along its edges:
/// a `Custom` one, which has no frame to be resized by, except on macOS,
/// which keeps one.
pub(super) fn host_resizes(style: TitleBarStyle) -> bool {
    style == TitleBarStyle::Custom && !cfg!(target_os = "macos")
}

/// The height of a `Custom` title bar when the guest names none, in logical
/// units: what Windows 11 and GNOME draw theirs at.
pub(super) const CUSTOM_HEIGHT: f64 = 32.0;

/// Within this long and this close, a second press is a double click where
/// the platform does not count clicks itself.
pub(super) const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(500);
pub(super) const DOUBLE_CLICK_DISTANCE: f64 = 4.0;

/// Whether `(x, y)` is inside one of `regions` (physical pixels).
pub(super) fn in_regions(regions: &[Rect], (x, y): (f64, f64)) -> bool {
    regions.iter().any(|rect| {
        let (left, top) = (f64::from(rect.x), f64::from(rect.y));
        x >= left
            && y >= top
            && x < left + f64::from(rect.width)
            && y < top + f64::from(rect.height)
    })
}

/// The edge or corner of a `size` window that `(x, y)` is within `band` of,
/// all in physical pixels. Corners take a band twice as long, as Windows'
/// own do.
pub(super) fn resize_edge(
    (x, y): (f64, f64),
    (width, height): (u32, u32),
    band: f64,
) -> Option<ResizeDirection> {
    let (width, height) = (f64::from(width), f64::from(height));
    if x < 0.0 || y < 0.0 || x >= width || y >= height {
        return None;
    }
    let corner = band * 2.0;
    let left = x < band;
    let right = x >= width - band;
    let top = y < band;
    let bottom = y >= height - band;
    let near_left = x < corner;
    let near_right = x >= width - corner;
    let near_top = y < corner;
    let near_bottom = y >= height - corner;
    Some(match () {
        _ if (top && near_left) || (left && near_top) => ResizeDirection::NorthWest,
        _ if (top && near_right) || (right && near_top) => ResizeDirection::NorthEast,
        _ if (bottom && near_left) || (left && near_bottom) => ResizeDirection::SouthWest,
        _ if (bottom && near_right) || (right && near_bottom) => ResizeDirection::SouthEast,
        _ if top => ResizeDirection::North,
        _ if bottom => ResizeDirection::South,
        _ if left => ResizeDirection::West,
        _ if right => ResizeDirection::East,
        _ => return None,
    })
}

/// The cursor that says an edge resizes.
pub(super) fn resize_cursor(edge: ResizeDirection) -> winit::window::CursorIcon {
    use winit::window::CursorIcon::*;
    match edge {
        ResizeDirection::North | ResizeDirection::South => NsResize,
        ResizeDirection::East | ResizeDirection::West => EwResize,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => NwseResize,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => NeswResize,
    }
}

/// What a double click on a title bar does. Only macOS has a setting for it;
/// elsewhere it maximizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) enum DoubleClick {
    /// Zoom (macOS) or maximize, and back.
    Zoom,
    Minimize,
    Nothing,
}

#[cfg(not(target_os = "macos"))]
pub(super) fn double_click_action() -> DoubleClick {
    DoubleClick::Zoom
}

/// How many clicks the press being handled makes, where the platform counts
/// them: `None` where it is up to the host to tell.
#[cfg(not(target_os = "macos"))]
pub(super) fn click_count(_window: &winit::window::Window) -> Option<u32> {
    None
}

#[cfg(target_os = "macos")]
pub(super) use mac::{click_count, double_click_action, layout_controls};

#[cfg(target_os = "macos")]
mod mac {
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplication, NSView, NSWindow, NSWindowButton};
    use objc2_foundation::{NSPoint, NSRect, NSString, NSUserDefaults};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::DoubleClick;

    fn ns_window(window: &winit::window::Window) -> Option<Retained<NSWindow>> {
        let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
            return None;
        };
        // SAFETY: winit's handle is its window's content view, alive while
        // the window is, and this runs on the main thread that owns it.
        let view: &NSView = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        view.window()
    }

    /// Where the traffic lights are, in the content view's points from its
    /// top-left corner, and the title bar they were placed in.
    #[derive(Clone, Copy, Debug)]
    pub(in super::super) struct Controls {
        /// The left edge of the leftmost button and the right edge of the
        /// rightmost one.
        pub left: f64,
        pub right: f64,
        /// How tall the title bar they sit in is.
        pub bar: f64,
    }

    /// Puts the traffic lights in the middle of a title bar `height` points
    /// tall (the system's usual one at 0), as far from the left edge as from
    /// the top, and says where they ended up. `None` while the window has no
    /// title bar to put them in (fullscreen) or none of them.
    pub(in super::super) fn layout_controls(
        window: &winit::window::Window,
        height: f64,
    ) -> Option<Controls> {
        let ns_window = ns_window(window)?;
        let buttons: Vec<_> = [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ]
        .into_iter()
        .filter_map(|kind| ns_window.standardWindowButton(kind))
        .collect();
        let first = buttons.first()?;
        // The title bar view, then the container AppKit sizes to the title
        // bar's height.
        // SAFETY: views are only read and changed on the main thread.
        let container = unsafe { first.superview()?.superview()? };
        let frame = ns_window.frame();
        let usual = frame.size.height - ns_window.contentLayoutRect().size.height;
        if usual <= 0.0 {
            return None;
        }
        let button = first.frame().size;
        let bar = if height >= button.height {
            height
        } else {
            usual
        };
        if bar != usual {
            let mut rect = container.frame();
            rect.size.height = bar;
            rect.origin.y = frame.size.height - bar;
            container.setFrame(rect);
            // As far in from the left as from the top; never closer than
            // AppKit's own place.
            let start = ((bar - button.height) / 2.0)
                .round()
                .max(first.frame().origin.x);
            let spacing = buttons.get(1).map_or(button.width + 6.0, |next| {
                next.frame().origin.x - first.frame().origin.x
            });
            for (index, button) in buttons.iter().enumerate() {
                let size = button.frame().size;
                button.setFrameOrigin(NSPoint::new(
                    start + spacing * index as f64,
                    ((bar - size.height) / 2.0).round(),
                ));
            }
        }
        let content = ns_window.contentView()?;
        let edges = buttons
            .iter()
            .filter(|button| !button.isHidden())
            .map(|button| {
                let rect: NSRect = button.convertRect_toView(button.bounds(), Some(&content));
                (rect.origin.x, rect.origin.x + rect.size.width)
            });
        let (left, right) = edges.fold((f64::MAX, f64::MIN), |(left, right), (l, r)| {
            (left.min(l), right.max(r))
        });
        (left <= right).then_some(Controls { left, right, bar })
    }

    /// How many clicks the press being handled makes, as AppKit counts them
    /// with the user's double-click speed.
    pub(in super::super) fn click_count(_window: &winit::window::Window) -> Option<u32> {
        let mtm = MainThreadMarker::new()?;
        let event = NSApplication::sharedApplication(mtm).currentEvent()?;
        u32::try_from(event.clickCount()).ok()
    }

    /// What the user set double clicks on title bars to do (System Settings,
    /// Desktop & Dock).
    pub(in super::super) fn double_click_action() -> DoubleClick {
        let defaults = NSUserDefaults::standardUserDefaults();
        let action = defaults
            .stringForKey(&NSString::from_str("AppleActionOnDoubleClick"))
            .map(|value| value.to_string());
        match action.as_deref() {
            Some("Minimize") => DoubleClick::Minimize,
            Some("None") => DoubleClick::Nothing,
            Some(_) => DoubleClick::Zoom,
            // Before the setting existed, a boolean said whether to minimize.
            None if defaults.boolForKey(&NSString::from_str("AppleMiniaturizeOnDoubleClick")) => {
                DoubleClick::Minimize
            }
            None => DoubleClick::Zoom,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_hold_their_top_left_edges_and_not_the_others() {
        let regions = [Rect {
            x: 10,
            y: 0,
            width: 20,
            height: 5,
        }];
        assert!(in_regions(&regions, (10.0, 0.0)));
        assert!(in_regions(&regions, (29.9, 4.9)));
        assert!(!in_regions(&regions, (30.0, 2.0)));
        assert!(!in_regions(&regions, (12.0, 5.0)));
        assert!(!in_regions(&[], (12.0, 2.0)));
    }

    #[test]
    fn only_a_custom_window_off_macos_is_resized_from_its_edges() {
        assert_eq!(
            host_resizes(TitleBarStyle::Custom),
            !cfg!(target_os = "macos")
        );
        assert!(!host_resizes(TitleBarStyle::Overlay));
        assert!(!host_resizes(TitleBarStyle::System));
    }

    #[test]
    fn an_edge_is_where_the_window_ends_now() {
        // The pointer on the right edge of a window 600 wide stays there
        // while the window grows to 1200: it is then in the content.
        assert_eq!(
            resize_edge((599.0, 100.0), (600, 400), 5.0),
            Some(ResizeDirection::East)
        );
        assert_eq!(resize_edge((599.0, 100.0), (1200, 400), 5.0), None);
    }

    #[test]
    fn edges_and_corners() {
        let size = (100, 80);
        assert_eq!(resize_edge((50.0, 40.0), size, 5.0), None);
        assert_eq!(
            resize_edge((50.0, 1.0), size, 5.0),
            Some(ResizeDirection::North)
        );
        assert_eq!(
            resize_edge((50.0, 79.0), size, 5.0),
            Some(ResizeDirection::South)
        );
        assert_eq!(
            resize_edge((0.0, 40.0), size, 5.0),
            Some(ResizeDirection::West)
        );
        assert_eq!(
            resize_edge((99.0, 40.0), size, 5.0),
            Some(ResizeDirection::East)
        );
        // Corners reach twice as far along each edge.
        assert_eq!(
            resize_edge((8.0, 1.0), size, 5.0),
            Some(ResizeDirection::NorthWest)
        );
        assert_eq!(
            resize_edge((1.0, 8.0), size, 5.0),
            Some(ResizeDirection::NorthWest)
        );
        assert_eq!(
            resize_edge((92.0, 1.0), size, 5.0),
            Some(ResizeDirection::NorthEast)
        );
        assert_eq!(
            resize_edge((1.0, 72.0), size, 5.0),
            Some(ResizeDirection::SouthWest)
        );
        assert_eq!(
            resize_edge((99.0, 79.0), size, 5.0),
            Some(ResizeDirection::SouthEast)
        );
        // Outside the window is nobody's.
        assert_eq!(resize_edge((-1.0, 40.0), size, 5.0), None);
        assert_eq!(resize_edge((100.0, 40.0), size, 5.0), None);
    }
}
