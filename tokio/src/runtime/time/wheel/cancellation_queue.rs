//! MPSC Intrusive Linked List
//!
//! This implementation is based on Dmitry Vyukov's [Intrusive MPSC node-based queue].
//!
//! [Intrusive MPSC node-based queue]: https://www.1024cores.net/home/lock-free-algorithms/queues/intrusive-mpsc-node-based-queue

use super::{Entry, EntryHandle};
use crate::loom::sync::{Arc, Mutex};

use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

#[derive(Debug)]
struct Inner {
    head: Option<NonNull<Entry>>,
    tail: Option<NonNull<Entry>>,
}

unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

impl Drop for Inner {
    fn drop(&mut self) {
        unsafe {
            while let Some(head) = self.head {
                self.head = head.as_ref().cancel_pointer().with(|p| *p);
                drop(EntryHandle::from(head));
            }
        }
    }
}

impl Inner {
    fn new() -> Self {
        Self {
            head: None,
            tail: None,
        }
    }

    fn push_back(&mut self, hdl: EntryHandle) {
        let entry = ManuallyDrop::new(hdl.into_entry());
        entry.cancel_pointer().with_mut(|p| {
            let p = unsafe { p.as_mut() }.unwrap();
            *p = None;
        });

        if self.head.is_none() {
            self.head = NonNull::new(Arc::as_ptr(&entry).cast_mut());
            self.tail = self.head;
        } else {
            let tail = self.tail.unwrap();
            unsafe {
                tail.as_ref().cancel_pointer().with_mut(|p| {
                    *p = Some(NonNull::new(Arc::as_ptr(&entry).cast_mut()).unwrap());
                });
            }
            self.tail = Some(NonNull::new(Arc::as_ptr(&entry).cast_mut()).unwrap());
        }
    }

    fn iter(&mut self) -> impl Iterator<Item = EntryHandle> {
        let mut head = self.head.take();
        let _ = self.tail.take();

        std::iter::from_fn(move || match head {
            Some(ptr) => {
                head = unsafe { ptr.as_ref() }
                    .cancel_pointer()
                    .with(|p| unsafe { *p });
                let hdl = EntryHandle::from(ptr);
                Some(hdl)
            }
            None => None,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Sender {
    inner: Arc<Mutex<Inner>>,
}

/// Safety: [`Sender`] is protected by [`AtomicPtr`]
unsafe impl Send for Sender {}

/// Safety: [`Sender`] is protected by [`AtomicPtr`]
unsafe impl Sync for Sender {}

impl Sender {
    pub(crate) unsafe fn send(&self, hdl: EntryHandle) {
        self.inner.lock().push_back(hdl);
    }
}

#[derive(Debug)]
pub(crate) struct Receiver {
    inner: Arc<Mutex<Inner>>,

    // make sure Receiver is `!Sync`
    _p: PhantomData<*const ()>,
}

/// Safety: [`Receiver`] can only be accessed from a single thread.
unsafe impl Send for Receiver {}

impl Receiver {
    pub(crate) unsafe fn recv_all(&mut self) -> impl Iterator<Item = EntryHandle> {
        self.inner.lock().iter()
    }
}

pub(crate) fn new() -> (Sender, Receiver) {
    let inner = Arc::new(Mutex::new(Inner::new()));
    (
        Sender {
            inner: inner.clone(),
        },
        Receiver {
            inner,
            _p: PhantomData,
        },
    )
}

#[cfg(test)]
mod tests;
