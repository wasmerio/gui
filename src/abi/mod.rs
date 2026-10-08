//! The guest ABI: how `gui.h` looks from inside a wasm32 module.
//!
//! Everything layout-shaped here is generated from `spec/gui.json`
//! (`codegen/gen.py`), and `guest/tests/abi_check.gen.c` makes the C compiler
//! confirm the generator's model. This module holds the hand-written pieces
//! the generated code builds on: bounds-checked guest memory access and the
//! small traits the readers implement.
//!
//! Nothing here depends on the target: the same glue serves a native host and
//! one compiled into a browser.

use std::marker::PhantomData;

use wasmer::MemoryView;

// The generated modules keep the layout their generator gives them.
#[allow(clippy::all, non_camel_case_types, clippy::upper_case_acronyms)]
#[rustfmt::skip]
#[path = "enums.gen.rs"]
pub mod enums;
#[allow(clippy::all)]
#[rustfmt::skip]
#[path = "structs.gen.rs"]
pub mod structs;
#[allow(clippy::all)]
#[rustfmt::skip]
#[path = "events.gen.rs"]
pub mod events;

#[allow(clippy::all, non_snake_case)]
#[rustfmt::skip]
#[path = "api.gen.rs"]
pub(crate) mod api;

#[rustfmt::skip]
#[path = "imports.gen.rs"]
mod imports;

pub use imports::*;

use enums::Status;

/// Core wasm value type of an import parameter or result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValType {
    I32,
    I64,
    F32,
    F64,
}

/// Name and core wasm signature of one import.
#[derive(Clone, Copy, Debug)]
pub struct ImportSig {
    pub name: &'static str,
    pub params: &'static [ValType],
    pub results: &'static [ValType],
    /// The import may wait: for the window system, or for input.
    pub blocking: bool,
}

/// Looks up an import of the GUI module by name.
pub fn import_sig(name: &str) -> Option<&'static ImportSig> {
    IMPORTS.iter().find(|sig| sig.name == name)
}

/// A guest object handle. Zero is never one.
pub type Handle = u32;

/// Largest array a descriptor may carry. Counts are guest-controlled, so a
/// ceiling keeps one call from sizing a huge host allocation.
pub const MAX_ARRAY_LENGTH: u32 = 1 << 16;
/// Largest string the host copies out of guest memory (titles, MIME types).
/// Clipboard contents are not strings and have a limit of their own.
pub const MAX_STRING_BYTES: u32 = 1 << 20;
/// Longest `nextInChain` list the host follows.
pub const MAX_CHAIN_LENGTH: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbiError {
    /// The guest passed an address range outside its linear memory.
    OutOfBounds {
        ptr: u32,
        len: u64,
    },
    InvalidEnum {
        ty: &'static str,
        value: u32,
    },
    /// A chained struct the host does not know, marked as not to be skipped.
    MustUnderstand(u32),
    ChainTooLong,
    ArrayTooLong(u32),
    StringTooLong,
    InvalidUtf8,
    /// A pointer the function needs was null.
    NullPointer(&'static str),
    /// The guest is being stopped: the call traps instead of returning.
    Stopped,
}

impl AbiError {
    /// What the guest is told. (An out-of-bounds access is not told: it traps.)
    pub fn status(&self) -> Status {
        match self {
            Self::MustUnderstand(_) => Status::Unsupported,
            _ => Status::InvalidArgument,
        }
    }
}

impl std::fmt::Display for AbiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfBounds { ptr, len } => {
                write!(f, "{len} bytes at {ptr:#x} are outside guest memory")
            }
            Self::InvalidEnum { ty, value } => write!(f, "{value:#x} is not a valid {ty}"),
            Self::MustUnderstand(s_type) => {
                write!(
                    f,
                    "sType {s_type:#x} must be understood and is not known here"
                )
            }
            Self::ChainTooLong => f.write_str("nextInChain list is too long"),
            Self::ArrayTooLong(count) => write!(f, "array of {count} elements is too long"),
            Self::StringTooLong => f.write_str("string is too long"),
            Self::InvalidUtf8 => f.write_str("string is not UTF-8"),
            Self::NullPointer(name) => write!(f, "`{name}` must not be null"),
            Self::Stopped => f.write_str("the program was stopped"),
        }
    }
}

impl std::error::Error for AbiError {}

pub type AbiResult<T> = Result<T, AbiError>;

/// An enum of the C API with its numeric values.
pub trait AbiEnum: Sized + Copy {
    const NAME: &'static str;
    fn from_u32(value: u32) -> Option<Self>;
    fn as_u32(self) -> u32;
    /// The value's name in the header, without the enum's prefix.
    fn name(self) -> &'static str;
}

/// What an import hands back to the guest.
///
/// Results are a status or nothing, and for the two functions that return a
/// value, zero is the answer a host without the function would give.
pub trait ImportResult: Sized {
    type Wasm;
    fn into_wasm(self) -> Self::Wasm;
    /// The result of a call whose arguments could not be decoded.
    fn from_error(error: &AbiError) -> Self;
}

impl ImportResult for Status {
    type Wasm = u32;
    fn into_wasm(self) -> u32 {
        self.as_u32()
    }
    fn from_error(error: &AbiError) -> Self {
        error.status()
    }
}

impl ImportResult for () {
    type Wasm = ();
    fn into_wasm(self) {}
    fn from_error(_: &AbiError) {}
}

impl ImportResult for u32 {
    type Wasm = u32;
    fn into_wasm(self) -> u32 {
        self
    }
    fn from_error(_: &AbiError) -> Self {
        0
    }
}

impl ImportResult for bool {
    type Wasm = u32;
    fn into_wasm(self) -> u32 {
        u32::from(self)
    }
    fn from_error(_: &AbiError) -> Self {
        false
    }
}

/// A struct that can be decoded from its wasm32 layout.
pub trait GuestRead: Sized {
    const SIZE: u32;
    fn read(mem: &GuestMemory<'_>, ptr: u32) -> AbiResult<Self>;
}

/// Something the host stores into guest memory.
pub trait GuestWrite {
    const SIZE: u32;
    fn write(&self, mem: &GuestMemory<'_>, ptr: u32) -> AbiResult<()>;
}

impl GuestWrite for u32 {
    const SIZE: u32 = 4;
    fn write(&self, mem: &GuestMemory<'_>, ptr: u32) -> AbiResult<()> {
        mem.write_bytes(ptr, &self.to_le_bytes())
    }
}

impl GuestWrite for u64 {
    const SIZE: u32 = 8;
    fn write(&self, mem: &GuestMemory<'_>, ptr: u32) -> AbiResult<()> {
        mem.write_bytes(ptr, &self.to_le_bytes())
    }
}

impl GuestWrite for f64 {
    const SIZE: u32 = 8;
    fn write(&self, mem: &GuestMemory<'_>, ptr: u32) -> AbiResult<()> {
        mem.write_bytes(ptr, &self.to_bits().to_le_bytes())
    }
}

/// An out-parameter: the guest address of a `T` the host fills in.
pub struct OutPtr<T> {
    pub ptr: u32,
    _marker: PhantomData<T>,
}

impl<T> Clone for OutPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for OutPtr<T> {}

impl<T> OutPtr<T> {
    pub fn new(ptr: u32) -> Self {
        Self {
            ptr,
            _marker: PhantomData,
        }
    }

    pub fn is_null(&self) -> bool {
        self.ptr == 0
    }
}

impl<T: GuestWrite> OutPtr<T> {
    pub fn write(&self, mem: &GuestMemory<'_>, value: &T) -> AbiResult<()> {
        value.write(mem, self.ptr)
    }

    /// Stores `value` unless the guest passed no pointer (for the optional
    /// out-parameters).
    pub fn write_if_present(&self, mem: &GuestMemory<'_>, value: &T) -> AbiResult<()> {
        if self.is_null() {
            return Ok(());
        }
        value.write(mem, self.ptr)
    }

    /// The `index`-th element of the array this points at.
    pub fn write_at(&self, mem: &GuestMemory<'_>, index: u32, value: &T) -> AbiResult<()> {
        let offset = u64::from(index) * u64::from(T::SIZE);
        let ptr = u64::from(self.ptr) + offset;
        let ptr =
            u32::try_from(ptr).map_err(|_| mem.fault(self.ptr, offset + u64::from(T::SIZE)))?;
        value.write(mem, ptr)
    }
}

/// Per-call context handed to every [`api`] method.
pub struct Cx<'a> {
    pub mem: GuestMemory<'a>,
    /// The instances the calling wasm instance created. They end with it.
    pub owned: &'a mut Vec<Handle>,
    /// Whether the guest is being stopped. A call that waits asks this now
    /// and then, and gives up with [`AbiError::Stopped`] when it says yes.
    pub stopped: &'a dyn Fn() -> bool,
}

/// Bounds-checked access to the calling instance's linear memory.
///
/// Guest memory is shared with every guest thread, so nothing here hands out
/// references into it: all reads and writes are copies.
pub struct GuestMemory<'a> {
    view: MemoryView<'a>,
    /// The first access that fell outside the memory, if any.
    fault: std::cell::Cell<Option<(u32, u64)>>,
}

impl<'a> GuestMemory<'a> {
    pub fn new(view: MemoryView<'a>) -> Self {
        Self {
            view,
            fault: std::cell::Cell::new(None),
        }
    }

    pub fn size(&self) -> u64 {
        self.view.data_size()
    }

    /// An access outside the memory. It is remembered as well as returned:
    /// whatever the caller makes of the error, the import ends in a trap (see
    /// [`GuestMemory::take_fault`]).
    fn fault(&self, ptr: u32, len: u64) -> AbiError {
        if self.fault.get().is_none() {
            self.fault.set(Some((ptr, len)));
        }
        AbiError::OutOfBounds { ptr, len }
    }

    /// The first out-of-bounds access made through this view, once. A wild
    /// guest pointer is a fault of the guest, never a status: the import that
    /// saw one traps even if its implementation went on.
    pub fn take_fault(&self) -> Option<AbiError> {
        self.fault
            .take()
            .map(|(ptr, len)| AbiError::OutOfBounds { ptr, len })
    }

    pub fn read_into(&self, ptr: u32, out: &mut [u8]) -> AbiResult<()> {
        self.view
            .read(u64::from(ptr), out)
            .map_err(|_| self.fault(ptr, out.len() as u64))
    }

    pub fn read_bytes<const N: usize>(&self, ptr: u32) -> AbiResult<[u8; N]> {
        let mut out = [0u8; N];
        self.read_into(ptr, &mut out)?;
        Ok(out)
    }

    pub fn read_vec(&self, ptr: u32, len: u64) -> AbiResult<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        if u64::from(ptr) + len > self.size() {
            return Err(self.fault(ptr, len));
        }
        let mut out = vec![0u8; len as usize];
        self.read_into(ptr, &mut out)?;
        Ok(out)
    }

    pub fn write_bytes(&self, ptr: u32, data: &[u8]) -> AbiResult<()> {
        self.view
            .write(u64::from(ptr), data)
            .map_err(|_| self.fault(ptr, data.len() as u64))
    }

    pub fn read_u32(&self, ptr: u32) -> AbiResult<u32> {
        Ok(u32::from_le_bytes(self.read_bytes::<4>(ptr)?))
    }

    /// Bytes of a string with non-null `data`: `length` of them, or up to the
    /// first NUL when `length` is `WGUI_STRLEN`.
    fn string_bytes(&self, data: u32, length: u32) -> AbiResult<Vec<u8>> {
        if length != enums::STRLEN {
            if length > MAX_STRING_BYTES {
                return Err(AbiError::StringTooLong);
            }
            return self.read_vec(data, u64::from(length));
        }
        // NUL-terminated: scan in chunks, never past the end of memory.
        let mut out = Vec::new();
        let mut at = u64::from(data);
        let end = self.size();
        let mut chunk = [0u8; 256];
        loop {
            if at >= end {
                return Err(self.fault(data, out.len() as u64 + 1));
            }
            let n = chunk.len().min((end - at) as usize);
            self.view
                .read(at, &mut chunk[..n])
                .map_err(|_| self.fault(data, out.len() as u64 + n as u64))?;
            if let Some(nul) = chunk[..n].iter().position(|&b| b == 0) {
                out.extend_from_slice(&chunk[..nul]);
                return Ok(out);
            }
            out.extend_from_slice(&chunk[..n]);
            if out.len() as u64 > u64::from(MAX_STRING_BYTES) {
                return Err(AbiError::StringTooLong);
            }
            at += n as u64;
        }
    }

    /// An input string. A null `data` is the empty string; anything that is
    /// not UTF-8 is an error rather than something to repair.
    pub fn read_string(&self, data: u32, length: u32) -> AbiResult<String> {
        if data == 0 {
            return Ok(String::new());
        }
        String::from_utf8(self.string_bytes(data, length)?).map_err(|_| AbiError::InvalidUtf8)
    }

    /// Follows a `nextInChain` list. This version defines no chained struct,
    /// so every link is one the host does not know: it is skipped, unless its
    /// sType says it must not be.
    pub fn check_chain(&self, mut ptr: u32) -> AbiResult<()> {
        let mut links = 0;
        while ptr != 0 {
            if links >= MAX_CHAIN_LENGTH {
                return Err(AbiError::ChainTooLong);
            }
            links += 1;
            let header = self.read_bytes::<8>(ptr)?;
            let s_type = rd_u32(&header, 4);
            if s_type & enums::STYPE_MUST_UNDERSTAND != 0 {
                return Err(AbiError::MustUnderstand(s_type));
            }
            ptr = rd_u32(&header, 0);
        }
        Ok(())
    }

    pub fn read_opt<T: GuestRead>(&self, ptr: u32) -> AbiResult<Option<T>> {
        if ptr == 0 {
            return Ok(None);
        }
        Ok(Some(T::read(self, ptr)?))
    }

    pub fn read_required<T: GuestRead>(&self, ptr: u32, name: &'static str) -> AbiResult<T> {
        if ptr == 0 {
            return Err(AbiError::NullPointer(name));
        }
        T::read(self, ptr)
    }

    fn check_array(&self, ptr: u32, count: u32, element: u32) -> AbiResult<()> {
        if count > MAX_ARRAY_LENGTH {
            return Err(AbiError::ArrayTooLong(count));
        }
        let len = u64::from(count) * u64::from(element);
        if count != 0 && u64::from(ptr) + len > self.size() {
            return Err(self.fault(ptr, len));
        }
        Ok(())
    }

    pub fn read_array<T: GuestRead>(&self, ptr: u32, count: u32) -> AbiResult<Vec<T>> {
        self.check_array(ptr, count, T::SIZE)?;
        (0..count)
            .map(|i| T::read(self, ptr + i * T::SIZE))
            .collect()
    }

    pub fn read_enum_array<E: AbiEnum>(&self, ptr: u32, count: u32) -> AbiResult<Vec<E>> {
        self.check_array(ptr, count, 4)?;
        let bytes = self.read_vec(ptr, u64::from(count) * 4)?;
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                let value = u32::from_le_bytes(*c);
                E::from_u32(value).ok_or(AbiError::InvalidEnum { ty: E::NAME, value })
            })
            .collect()
    }

    /// Copies `data` into a guest buffer of `capacity` bytes and stores its
    /// full length: the "how long is it, and here is what fits" convention of
    /// the getters that return text. Returns whether all of it fitted.
    pub fn write_truncated(
        &self,
        buffer: u32,
        capacity: u32,
        length: OutPtr<u32>,
        data: &[u8],
    ) -> AbiResult<bool> {
        let fits = data.len().min(capacity as usize);
        if buffer != 0 && fits != 0 {
            self.write_bytes(buffer, &data[..fits])?;
        }
        length.write(self, &(data.len() as u32))?;
        Ok(fits == data.len())
    }
}

pub(crate) fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

pub(crate) fn rd_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

pub(crate) fn wr_u32(b: &mut [u8], at: usize, value: u32) {
    b[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn wr_u64(b: &mut [u8], at: usize, value: u64) {
    b[at..at + 8].copy_from_slice(&value.to_le_bytes());
}
