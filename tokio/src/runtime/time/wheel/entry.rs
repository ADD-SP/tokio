use crate::loom::cell::UnsafeCell;
use crate::loom::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use crate::loom::sync::Mutex;
use crate::{sync::AtomicWaker, util::linked_list};
use std::sync::Arc;
use std::{ptr::NonNull, sync::mpsc, task::Waker};

pub(crate) type EntryList = linked_list::LinkedList<Entry, Entry>;

pub(crate) const STATE_UNREGISTERED: u8 = 0;
pub(crate) const STATE_REGISTERED: u8 = 1;
pub(crate) const STATE_PENDING: u8 = 2;
pub(crate) const STATE_PREMATURE: u8 = 3;
pub(crate) const MAX_SAFE_MILLIS_DURATION: u64 = u64::MAX - 1;

pub(crate) struct Entry {
    /// Intrusive list pointers.
    pointers: linked_list::Pointers<Entry>,

    state: AtomicU8,

    when: u64,

    cancel_tx: Mutex<Option<mpsc::Sender<Handle>>>,

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
        let mut lock = self.cancel_tx.lock();
        assert!(lock.is_none(), "Cancel channel already set");
        *lock = Some(cancel_tx);
    }

    pub(crate) fn handle(&self) -> &Handle {
        &self.handle
    }

    pub(crate) fn when(&self) -> u64 {
        self.when
    }

    pub(crate) fn transition_to_registered(&self) {
        let old = self.state.swap(STATE_REGISTERED, Ordering::Relaxed);
        assert_eq!(old, STATE_UNREGISTERED, "Entry already registered");
    }

    pub(crate) fn transition_to_pending(&self, not_after: u64) -> Result<(), u64> {
        if self.when > not_after {
            return Err(self.when);
        }
        let old = self.state.swap(STATE_PENDING, Ordering::Relaxed);
        assert_eq!(old, STATE_REGISTERED, "Entry not registered");
        Ok(())
    }

    pub(crate) fn fire(&self) {
        let state = self.state.fetch_or(0, Ordering::Relaxed);
        assert_eq!(state, STATE_PENDING, "Entry not in pending state");
        self.waker.wake();
    }

    pub(crate) fn fire_unregistered(&self) {
        // let state = self.state.fetch_or(0, Ordering::Relaxed);
        // assert_eq!(state, STATE_UNREGISTERED, "Entry not in unregistered state");
        self.waker.wake();
        let old = self.state.swap(STATE_PREMATURE, Ordering::Release);
        assert_eq!(old, STATE_UNREGISTERED, "Entry state changed unexpectedly");
    }

    pub(crate) fn is_elapsed(&self) -> bool {
        let state = self.state.fetch_or(0, Ordering::Relaxed);
        state == STATE_PENDING || state == STATE_PREMATURE
    }

    pub(crate) fn is_registered(&self) -> bool {
        self.state.fetch_or(0, Ordering::Relaxed) == STATE_REGISTERED
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.state.fetch_or(0, Ordering::Relaxed) == STATE_PENDING
    }

    pub(crate) fn is_premature(&self) -> bool {
        self.state.fetch_or(0, Ordering::Relaxed) == STATE_PREMATURE
    }

    pub(crate) fn cancel(&self) {
        if self.is_registered() {
            if let Some(tx) = self.cancel_tx.lock().take() {
                tx.send(self.handle.clone())
                    .expect("Failed to send cancel message");
            }
        }
    }
}

pub(crate) struct Handle {
    refs: Arc<AtomicUsize>,
    entry: NonNull<Entry>,
}

impl std::fmt::Display for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Handle({:p}, {})",
            self.entry.as_ptr(),
            self.refs.fetch_or(0, Ordering::Relaxed)
        )
    }
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

pub(crate) fn new(when: u64, waker: &Waker, cancel_tx: Option<mpsc::Sender<Handle>>) -> Handle {
    let refs = Arc::new(AtomicUsize::new(2));

    let mut entry = Box::new(Entry {
        pointers: linked_list::Pointers::new(),
        state: AtomicU8::new(STATE_UNREGISTERED),
        when,
        cancel_tx: Mutex::new(cancel_tx),
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
