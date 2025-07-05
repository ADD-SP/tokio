use std::{ptr::NonNull, task::Waker, sync::mpsc};
use crate::{sync::AtomicWaker, util::linked_list};
use crate::loom::sync::atomic::{AtomicU64, Ordering};

pub(crate) type EntryList = linked_list::LinkedList<Entry, Entry>;

pub(crate) const STATE_PENDING: u64 = u64::MAX;
pub(crate) const MAX_SAFE_MILLIS_DURATION: u64 = STATE_PENDING - 1;

pub(crate) struct Entry {
    /// Intrusive list pointers.
    pointers: linked_list::Pointers<Entry>,

    registered_when: AtomicU64,

    cancel_tx: Option<mpsc::Sender<Handle>>,

    /// The currently-registered waker.
    waker: AtomicWaker,
}

generate_addr_of_methods! {
    impl<> Entry {
        unsafe fn addr_of_pointers(self: NonNull<Self>) -> NonNull<linked_list::Pointers<Entry>> {
            &self.pointers
        }
    }
}

unsafe impl linked_list::Link for Entry {
    type Handle = Handle;
    type Target = Entry;

    fn as_raw(handle: &Self::Handle) -> NonNull<Self::Target> {
        handle.entry
    }

    unsafe fn from_raw(ptr: NonNull<Self::Target>) -> Self::Handle {
        Handle {
            entry: ptr,
        }
    }

    unsafe fn pointers(
        target: NonNull<Self::Target>,
    ) -> NonNull<linked_list::Pointers<Self::Target>> {
        Entry::addr_of_pointers(target)
    }
}

impl Entry {
    pub(crate) fn new(when: u64, cancel_tx: Option<mpsc::Sender<Handle>>) -> Self {
        Entry {
            pointers: linked_list::Pointers::new(),
            registered_when: AtomicU64::new(when),
            cancel_tx,
            waker: AtomicWaker::new(),
        }
    }

    pub(crate) fn register_waker(&self, waker: &Waker) {
        self.waker.register_by_ref(waker);
    }

    pub(crate) fn handle(&self) -> Handle {
        Handle {
            entry: NonNull::from(self),
        }
    }

    pub(crate) fn registered_when(&self) -> u64 {
        self.registered_when.fetch_add(0, Ordering::Relaxed)
    }

    pub(crate) fn set_registered_when(&self, when: u64) {
        self.registered_when.store(when, Ordering::Relaxed);
    }

    pub(crate) fn try_mark_pending(&self, not_after: u64) -> Result<(), u64> {
        let mut cur = self.registered_when.load(Ordering::Relaxed);
        loop {
            if cur > not_after {
                return Err(cur);
            }

            match self.registered_when.compare_exchange_weak(
                cur,
                STATE_PENDING,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => cur = actual,
            }
        }
    }

    pub(crate) fn fire(&self) {
        self.waker.wake();
    }
}

pub(crate) struct Handle {
    entry: NonNull<Entry>,
}

unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    /// # Safety
    ///
    /// The caller must ensure that the associated `Entry` is not
    /// deallocated.
    pub(crate) fn register_waker(&self, waker: &Waker) {
        unsafe { self.entry.as_ref().register_waker(waker) }
    }

    /// # Safety
    ///
    /// The caller must ensure that the associated `Entry` is not
    /// deallocated.
    pub(crate) unsafe fn registered_when(&self) -> u64 {
        unsafe { self.entry.as_ref().registered_when() }
    }

    /// # Safety
    ///
    /// The caller must ensure that the associated `Entry` is not
    /// deallocated.
    pub(crate) unsafe fn set_registered_when(&self, when: u64) {
        unsafe { self.entry.as_ref().set_registered_when(when) }
    }

    pub(crate) fn elapsed(&self) -> bool {
        todo!()
    }

    pub(crate) fn try_mark_pending(&self, not_after: u64) -> Result<(), u64> {
        unsafe { self.entry.as_ref().try_mark_pending(not_after) }
    }

    pub(crate) fn fire(&self) {
        unsafe { self.entry.as_ref().fire() }
    }
}

impl From<Handle> for NonNull<Entry> {
    fn from(handle: Handle) -> Self {
        handle.entry
    }
}
