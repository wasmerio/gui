//! The handle table: the guest's `WGUI*` handles, and what they refer to.
//!
//! This is the safety boundary of the host. The guest only ever holds 32-bit
//! handles; each is checked for liveness (a generation tag catches stale ones)
//! and for type before anything is touched, so a confused or hostile guest
//! can at worst be told `WGUIStatus_InvalidHandle`.

use std::sync::Arc;

use super::{Cursor, Display, Instance, Window};
use crate::abi::Handle;

macro_rules! objects {
    ($( $variant:ident($state:ty) ),* $(,)?) => {
        /// What a handle refers to. Cloning is a reference-count bump.
        #[derive(Clone)]
        pub(crate) enum Object {
            $( $variant(Arc<$state>), )*
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum ObjectKind {
            $( $variant, )*
        }

        impl Object {
            pub(crate) fn kind(&self) -> ObjectKind {
                match self {
                    $( Object::$variant(_) => ObjectKind::$variant, )*
                }
            }
        }
    };
}

objects! {
    Instance(Instance),
    Window(Window),
    Display(Display),
    Cursor(Cursor),
}

struct Entry {
    object: Object,
    /// The instance the object was made on: destroying that destroys this.
    /// An instance's own entry holds 0.
    instance: Handle,
}

struct Slot {
    generation: u16,
    entry: Option<Entry>,
}

const INDEX_BITS: u32 = 20;
const INDEX_MASK: u32 = (1 << INDEX_BITS) - 1;
const GENERATION_MASK: u16 = (1 << (32 - INDEX_BITS)) - 1;

#[derive(Default)]
pub(super) struct Table {
    slots: Vec<Slot>,
    free: Vec<u32>,
}

impl Table {
    fn handle(index: u32, generation: u16) -> Handle {
        (u32::from(generation) << INDEX_BITS) | (index + 1)
    }

    fn entry(&self, handle: Handle) -> Option<&Entry> {
        let index = (handle & INDEX_MASK).checked_sub(1)? as usize;
        let slot = self.slots.get(index)?;
        if u32::from(slot.generation) != handle >> INDEX_BITS {
            return None;
        }
        slot.entry.as_ref()
    }

    /// Stores `object` and returns its handle, or `None` when the table has
    /// no room for more.
    pub(super) fn insert(&mut self, object: Object, instance: Handle) -> Option<Handle> {
        let index = match self.free.pop() {
            Some(index) => index,
            None => {
                if self.slots.len() as u32 >= INDEX_MASK {
                    return None;
                }
                self.slots.push(Slot {
                    generation: 0,
                    entry: None,
                });
                self.slots.len() as u32 - 1
            }
        };
        let slot = &mut self.slots[index as usize];
        slot.entry = Some(Entry { object, instance });
        Some(Self::handle(index, slot.generation))
    }

    pub(super) fn get(&self, handle: Handle) -> Option<Object> {
        self.entry(handle).map(|entry| entry.object.clone())
    }

    /// The instance `handle` was made on (0 for an instance, or for nothing).
    pub(super) fn instance_of(&self, handle: Handle) -> Handle {
        self.entry(handle).map_or(0, |entry| entry.instance)
    }

    /// Takes `handle` out of the table if it is live and of this kind.
    pub(super) fn remove(&mut self, handle: Handle, kind: ObjectKind) -> Option<Object> {
        if self.entry(handle)?.object.kind() != kind {
            return None;
        }
        let index = (handle & INDEX_MASK) - 1;
        let slot = &mut self.slots[index as usize];
        let entry = slot.entry.take()?;
        // The next occupant of this slot gets a different handle value, so a
        // stale handle is detected instead of aliasing the new object.
        slot.generation = (slot.generation + 1) & GENERATION_MASK;
        self.free.push(index);
        Some(entry.object)
    }

    /// Takes out everything that was made on `instance`.
    pub(super) fn remove_made_on(&mut self, instance: Handle) -> Vec<Object> {
        let handles: Vec<(Handle, ObjectKind)> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                let entry = slot.entry.as_ref()?;
                (entry.instance == instance).then(|| {
                    (
                        Self::handle(index as u32, slot.generation),
                        entry.object.kind(),
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|(handle, kind)| self.remove(handle, kind))
            .collect()
    }

    /// Takes out everything.
    pub(super) fn clear(&mut self) -> Vec<Object> {
        let mut objects = Vec::new();
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(entry) = slot.entry.take() {
                slot.generation = (slot.generation + 1) & GENERATION_MASK;
                self.free.push(index as u32);
                objects.push(entry.object);
            }
        }
        objects
    }

    /// Everything of one kind.
    pub(super) fn all(&self, kind: ObjectKind) -> Vec<Object> {
        self.slots
            .iter()
            .filter_map(|slot| slot.entry.as_ref())
            .filter(|entry| entry.object.kind() == kind)
            .map(|entry| entry.object.clone())
            .collect()
    }

    pub(super) fn count(&self, kind: ObjectKind) -> usize {
        self.slots
            .iter()
            .filter(|slot| {
                slot.entry
                    .as_ref()
                    .is_some_and(|entry| entry.object.kind() == kind)
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor() -> Object {
        Object::Cursor(Arc::new(Cursor {
            image: Arc::new(crate::system::CursorImage {
                width: 1,
                height: 1,
                hotspot_x: 0,
                hotspot_y: 0,
                scale_factor: 1.0,
                rgba: vec![0; 4],
            }),
        }))
    }

    #[test]
    fn handles_are_nonzero_and_typed() {
        let mut table = Table::default();
        let handle = table.insert(cursor(), 9).expect("room");
        assert_ne!(handle, 0);
        assert_eq!(table.instance_of(handle), 9);
        assert!(
            table.remove(handle, ObjectKind::Window).is_none(),
            "wrong type"
        );
        assert!(table.get(handle).is_some());
        assert!(table.remove(handle, ObjectKind::Cursor).is_some());
        assert!(table.get(handle).is_none());
        assert_eq!(table.count(ObjectKind::Cursor), 0);
    }

    #[test]
    fn a_stale_handle_does_not_alias_the_slot_s_next_object() {
        let mut table = Table::default();
        let first = table.insert(cursor(), 0).expect("room");
        table.remove(first, ObjectKind::Cursor).expect("live");
        let second = table.insert(cursor(), 0).expect("room");
        assert_ne!(first, second, "the slot is reused under a new generation");
        assert!(table.get(first).is_none());
        assert!(table.get(second).is_some());
        assert!(table.remove(first, ObjectKind::Cursor).is_none());
    }

    #[test]
    fn garbage_handles_are_rejected() {
        let mut table = Table::default();
        table.insert(cursor(), 0).expect("room");
        for handle in [0, 2, 0xFFFF_FFFF, 0x0010_0001] {
            assert!(table.get(handle).is_none(), "{handle:#x}");
            assert!(table.remove(handle, ObjectKind::Cursor).is_none());
        }
    }

    #[test]
    fn what_was_made_on_an_instance_goes_with_it() {
        let mut table = Table::default();
        let ours = table.insert(cursor(), 5).expect("room");
        let theirs = table.insert(cursor(), 6).expect("room");
        assert_eq!(table.remove_made_on(5).len(), 1);
        assert!(table.get(ours).is_none());
        assert!(table.get(theirs).is_some());
        assert_eq!(table.clear().len(), 1);
        assert!(table.get(theirs).is_none());
    }
}
