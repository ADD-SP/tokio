use std::sync::Arc;
use std::{ptr::NonNull, task::Waker, sync::mpsc};
use crate::{sync::AtomicWaker, util::linked_list};
use crate::loom::cell::UnsafeCell;
use crate::loom::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub(crate) type EntryList = linked_list::LinkedList<Entry, Entry>;

pub(crate) const STATE_PENDING: u64 = u64::MAX;
pub(crate) const STATE_CANCELLED: u64 = STATE_PENDING - 1;
pub(crate) const MAX_SAFE_MILLIS_DURATION: u64 = STATE_CANCELLED - 1;

pub(crate) struct Entry {
    /// Intrusive list pointers.
    pointers: linked_list::Pointers<Entry>,

    registered_when: AtomicU64,

    cancel_tx: UnsafeCell<Option<mpsc::Sender<Handle>>>,

    /// The currently-registered waker.
    waker: AtomicWaker,

    handle: Handle,
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
        ptr.as_ref().handle.clone()
    }

    unsafe fn pointers(
        target: NonNull<Self::Target>,
    ) -> NonNull<linked_list::Pointers<Self::Target>> {
        Entry::addr_of_pointers(target)
    }
}

impl Entry {
    pub(crate) fn register_waker(&self, waker: &Waker) {
        self.waker.register_by_ref(waker);
    }

    pub(crate) fn set_cancel_tx(&self, cancel_tx: mpsc::Sender<Handle>) {
        let _ = self.registered_when.fetch_add(0, Ordering::Acquire);
        let old = self.cancel_tx.with_mut(|tx| {
            let tx = unsafe { tx.as_mut() }.unwrap();
            tx.replace(cancel_tx)
        });
        assert!(old.is_none(), "Entry already has a cancel channel");
        let _ = self.registered_when.fetch_add(0, Ordering::Release);
    }

    pub(crate) fn handle(&self) -> &Handle {
        &self.handle
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

    pub(crate) fn is_elapsed(&self) -> bool {
        self.registered_when.fetch_add(0, Ordering::Relaxed) == STATE_PENDING
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.registered_when.fetch_add(0, Ordering::Relaxed) == STATE_CANCELLED
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.registered_when.fetch_add(0, Ordering::Relaxed) == STATE_PENDING
    }

    pub(crate) fn cancel(&self) {
        let _ = self.registered_when.fetch_add(0, Ordering::Acquire);
        self.cancel_tx.with_mut(|tx| {
            let tx = unsafe { tx.as_mut() }.unwrap();
            if let Some(tx) = tx.take() {
                // If we have a cancel channel, send the handle to it.
                tx.send(self.handle().clone()).expect("Failed to send cancel message");
            }
        });
        let _ = self.registered_when.fetch_add(0, Ordering::Release);
    }
}

pub(crate) struct Handle {
    refs: Arc<AtomicUsize>,
    entry: NonNull<Entry>,
}

impl Clone for Handle {
    fn clone(&self) -> Self {
        self.refs.fetch_add(1, Ordering::Relaxed);
        Handle {
            refs: self.refs.clone(),
            entry: self.entry,
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // `refs == 2` means this is the last handle except another one
        // in the entry itself.
        if self.refs.fetch_sub(1, Ordering::Release) == 2 {
            unsafe {
                std::ptr::drop_in_place(self.entry.as_ptr());
            }
        }
    }
}

impl std::ops::Deref for Handle {
    type Target = Entry;

    fn deref(&self) -> &Self::Target {
        unsafe { self.entry.as_ref() }
    }
}

impl From<Handle> for NonNull<Entry> {
    fn from(handle: Handle) -> Self {
        handle.entry
    }
}

unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

pub(crate) fn new(
    when: u64,
    waker: &Waker,
    cancel_tx: Option<mpsc::Sender<Handle>>,
) -> Handle {
    let refs = Arc::new(AtomicUsize::new(2));

    let mut entry = Box::new(Entry {
        pointers: linked_list::Pointers::new(),
        registered_when: AtomicU64::new(when),
        cancel_tx: UnsafeCell::new(cancel_tx),
        waker: AtomicWaker::new(),
        handle: Handle {
            refs: refs.clone(),
            entry: NonNull::dangling(), // Will be set later
        },
    });

    entry.handle.entry = NonNull::from(entry.as_ref());
    entry.register_waker(waker);
    let entry_ptr = NonNull::from(Box::leak(entry));

    Handle {
        refs,
        entry: entry_ptr,
    }
}
