//! `wguiInstanceSetIcon`: the program's icon.
//!
//! A guest offers its icon as images, raw or as PNG files, some of them made
//! for particular platforms. All of them are checked here, on the guest's
//! thread; those for the window system's platform are decoded, and that is
//! all the window system is handed ([`Icon`]): it neither parses files nor
//! chooses between platforms.

use std::io::Cursor;
use std::sync::Arc;

use super::{Host, Instance, Object, ObjectKind, Window, lock};
use crate::abi::enums::*;
use crate::abi::structs::IconImage as GuestImage;
use crate::abi::{AbiResult, Cx, GuestMemory, Handle};
use crate::system::{Icon, IconImage, WindowChange};

/// Images one call may offer.
const MAX_IMAGES: usize = 32;
/// The longest edge of an image: macOS's largest app icon.
const MAX_EDGE: u32 = 1024;
/// What is kept for the window system's platform: the files, and then the
/// pixels, four bytes each. No one image may be larger either.
const MAX_BYTES: u64 = 16 << 20;
/// What the decoder may allocate besides the image: rows, and the chunks it
/// reads on the way.
const DECODER_BYTES: usize = 16 << 20;
/// How every PNG file starts: its signature, then the length and type of
/// the IHDR chunk, whose first eight bytes are the width and the height.
const PNG_START: [u8; 16] = [
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
];

/// Whether two of an instance's icons are the one same setting.
pub(crate) fn same(a: &Option<Arc<Icon>>, b: &Option<Arc<Icon>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// Gives `windows` the instance's icon, `icon` first. The icon is not held
/// while windows are told, so another thread may set another meanwhile, and
/// one of the two be told last: this tells them again until what they were
/// last told is what the instance has.
pub(super) fn show_on(instance: &Instance, windows: &[Arc<Window>], mut icon: Option<Arc<Icon>>) {
    loop {
        for window in windows {
            window.apply(WindowChange::Icon(icon.clone()));
        }
        let now = lock(&instance.icon).clone();
        if same(&now, &icon) {
            return;
        }
        icon = now;
    }
}

impl Host {
    pub(super) fn set_icon(
        &self,
        cx: &mut Cx<'_>,
        instance: Handle,
        images: &[GuestImage],
    ) -> AbiResult<Status> {
        let Some(state) = self.instance(instance) else {
            return Ok(Status::InvalidHandle);
        };
        if !self.has(FeatureName::AppIcon) {
            return Ok(Status::Unsupported);
        }
        let icon = if images.is_empty() {
            None
        } else {
            match self.read_icon(&cx.mem, images)? {
                Ok(icon) => Some(Arc::new(icon)),
                Err(status) => return Ok(status),
            }
        };
        *lock(&state.icon) = icon.clone();
        show_on(&state, &self.windows_of(&state), icon);
        Ok(Status::Success)
    }

    /// Checks every image, and decodes the ones the window system is to
    /// choose from.
    fn read_icon(
        &self,
        mem: &GuestMemory<'_>,
        images: &[GuestImage],
    ) -> AbiResult<Result<Icon, Status>> {
        if images.len() > MAX_IMAGES {
            return Ok(Err(Status::LimitExceeded));
        }
        for image in images {
            if let Err(status) = check(mem, image)? {
                return Ok(Err(status));
            }
        }
        // The versions made for this platform, or else those made for every
        // platform. Bits this version does not know name platforms it is
        // not: a guest built against a newer header may set them.
        let platform = self.ctx.system.info().platform;
        let made_for_here = |image: &&GuestImage| platform != 0 && image.platforms & platform != 0;
        let mut kept: Vec<&GuestImage> = images.iter().filter(made_for_here).collect();
        if kept.is_empty() {
            kept = images.iter().filter(|image| image.platforms == 0).collect();
        }
        if kept.is_empty() {
            return Ok(Err(Status::NotFound));
        }
        let files: u64 = kept.iter().map(|image| u64::from(image.size)).sum();
        let pixels: u64 = kept
            .iter()
            .map(|image| u64::from(image.width) * u64::from(image.height) * 4)
            .sum();
        if files > MAX_BYTES || pixels > MAX_BYTES {
            return Ok(Err(Status::LimitExceeded));
        }
        let mut decoded = Vec::with_capacity(kept.len());
        for image in kept {
            // Read once: what is decoded is this copy, which the guest's
            // other threads cannot change under the decoder.
            let data = mem.read_vec(image.data, u64::from(image.size))?;
            let rgba = match image.format {
                IconFormat::Rgba8 => data,
                IconFormat::Png => match decode_png(&data, image.width, image.height) {
                    Some(rgba) => rgba,
                    None => return Ok(Err(Status::InvalidArgument)),
                },
            };
            decoded.push(IconImage {
                width: image.width,
                height: image.height,
                rgba,
            });
        }
        // The largest first, as `Icon::image_for` expects.
        decoded.sort_by_key(|image| std::cmp::Reverse(image.width));
        Ok(Ok(Icon { images: decoded }))
    }

    /// The windows opened on an instance that are still open.
    fn windows_of(&self, instance: &Instance) -> Vec<Arc<Window>> {
        let windows = lock(&self.table).all(ObjectKind::Window);
        windows
            .into_iter()
            .filter_map(|object| match object {
                Object::Window(window) if Arc::ptr_eq(&window.instance, &instance.shared) => {
                    Some(window)
                }
                _ => None,
            })
            .collect()
    }
}

/// Whether an image says what it is truthfully, as far as can be told
/// without decoding it: its size, and for a PNG file the start of the file.
fn check(mem: &GuestMemory<'_>, image: &GuestImage) -> AbiResult<Result<(), Status>> {
    let (width, height) = (image.width, image.height);
    if width == 0 || width != height || image.data == 0 || image.size == 0 {
        return Ok(Err(Status::InvalidArgument));
    }
    if width > MAX_EDGE || u64::from(image.size) > MAX_BYTES {
        return Ok(Err(Status::LimitExceeded));
    }
    // All of it is the guest's to point at, whether or not it is read.
    mem.check_range(image.data, u64::from(image.size))?;
    let truthful = match image.format {
        IconFormat::Rgba8 => u64::from(image.size) == u64::from(width) * u64::from(height) * 4,
        IconFormat::Png => {
            image.size >= 24 && {
                let start = mem.read_bytes::<24>(image.data)?;
                let number = |at: usize| {
                    u32::from_be_bytes([start[at], start[at + 1], start[at + 2], start[at + 3]])
                };
                start[..16] == PNG_START && number(16) == width && number(20) == height
            }
        }
    };
    Ok(if truthful {
        Ok(())
    } else {
        Err(Status::InvalidArgument)
    })
}

/// A PNG file as RGBA, 8 bits each, straight alpha, if it is one of the
/// size it says. Its default image, if it is animated.
fn decode_png(file: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let mut decoder = png::Decoder::new(Cursor::new(file));
    // Palettes and low bit depths expanded, 16 bits cut to 8.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    decoder.set_limits(png::Limits {
        bytes: DECODER_BYTES,
    });
    let mut reader = decoder.read_info().ok()?;
    // Nothing is made for a file that says it is larger than it was said to
    // be: its size is what bounds the buffer.
    if reader.info().size() != (width, height) {
        return None;
    }
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buffer).ok()?;
    if (frame.width, frame.height) != (width, height) || frame.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let pixels = buffer.get(..frame.buffer_size())?;
    Some(match frame.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Rgb => pixels
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 0xFF])
            .collect(),
        png::ColorType::GrayscaleAlpha => pixels
            .chunks_exact(2)
            .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
            .collect(),
        png::ColorType::Grayscale => pixels
            .iter()
            .flat_map(|&value| [value, value, value, 0xFF])
            .collect(),
        // Expanded to Rgb or Rgba above.
        png::ColorType::Indexed => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::decode_png;

    /// A PNG file's start that says it is `width` by `height`, RGBA.
    fn start(width: u32, height: u32) -> Vec<u8> {
        let mut chunk = b"IHDR".to_vec();
        chunk.extend_from_slice(&width.to_be_bytes());
        chunk.extend_from_slice(&height.to_be_bytes());
        chunk.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut file = super::PNG_START[..8].to_vec();
        file.extend_from_slice(&13u32.to_be_bytes());
        file.extend_from_slice(&chunk);
        let crc = crc32(&chunk);
        file.extend_from_slice(&crc.to_be_bytes());
        file
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn a_file_larger_than_it_was_said_to_be_is_not_decoded() {
        // A guest thread can change the file after it was checked: what is
        // decoded must still be the size checked, or nothing is allocated.
        assert_eq!(decode_png(&start(4_000_000, 2_000_000_000), 2, 2), None);
        assert_eq!(decode_png(&start(4, 4), 2, 2), None);
    }
}
