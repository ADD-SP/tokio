use super::wheel::EntryHandle;
use crate::{
    runtime::{context, time::{wheel::Entry, Wheel}},
    time::Instant,
};
use std::{
    mem::ManuallyDrop, pin::Pin, sync::mpsc, task::{Context, Poll}
};

pub(crate) struct Timer {
    /// The entry in the timing wheel.
    ///
    /// This is `None` if the timer has been deregistered.
    entry: Option<EntryHandle>,

    /// The deadline for the timer.
    deadline: Instant,
}

impl std::fmt::Debug for Timer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timer")
            .field("deadline", &self.deadline)
            .finish()
    }
}

impl Timer {
    pub(crate) fn new(deadline: Instant) -> Self {
        Timer {
            entry: None,
            deadline,
        }
    }

    fn register(self: Pin<&mut Self>) {
        use crate::runtime::scheduler;

        let this = self.get_mut();

        with_current_wheel(|maybe_wheel| {
            let when = scheduler::Handle::current().driver().time().time_source().instant_to_tick(this.deadline);
            if let Some((wheel, tx)) = maybe_wheel {
                let entry = Entry::new(when, Some(tx));
                let hdl = entry.handle();
                unsafe {
                    wheel.insert(entry)
                };
                this.entry = Some(hdl);
            } else {
                let entry = ManuallyDrop::new(Entry::new(when, None));
                push_inject(entry.handle());
            }
        });
    }

    pub(crate) fn poll_elapsed(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        match self.entry.as_ref() {
            Some(entry) if entry.elapsed() => Poll::Ready(()),
            Some(entry) => {
                entry.register_waker(cx.waker());
                Poll::Pending
            }
            None => {
                self.register();
                Poll::Pending
            }
        }
    }
}

fn with_current_wheel<F, R>(f: F) -> R
where
    F: FnOnce(Option<(&mut Wheel, mpsc::Sender<EntryHandle>)>) -> R,
{
    use crate::runtime::scheduler::Context::{CurrentThread, MultiThread};

    context::with_scheduler(|maybe_cx| match maybe_cx {
        Some(CurrentThread(cx)) => cx.with_wheel(f),
        Some(MultiThread(cx)) => cx.with_wheel(f),
        None => f(None),
    })
}

fn push_inject(hdl: EntryHandle) {
    use crate::runtime::scheduler::Handle::{CurrentThread, MultiThread};

    context::with_current(|sched_hdl| match sched_hdl {
        CurrentThread(sched_hdl) => sched_hdl.push_remote_timer(hdl),
        MultiThread(sched_hdl) => sched_hdl.push_remote_timer(hdl),
    }).unwrap();
}
