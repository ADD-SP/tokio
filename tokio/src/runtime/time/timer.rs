use super::wheel::EntryHandle;
use crate::{
    runtime::{context, time::{wheel::entry, Wheel}},
    time::Instant,
};
use std::{
    pin::Pin, sync::mpsc, task::{Context, Poll}
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

impl Drop for Timer {
    fn drop(&mut self) {
        Pin::new(self).cancel();
    }
}

impl Timer {
    pub(crate) fn new(deadline: Instant) -> Self {
        // dbg!("Creating timer with deadline: {:?}", deadline);
        Timer {
            entry: None,
            deadline,
        }
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(crate) fn is_elapsed(&self) -> bool {
        self.entry.as_ref().map_or(false, |entry| entry.is_elapsed())
    }

    fn register(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        use crate::runtime::scheduler;

        let this = self.get_mut();

        with_current_wheel(|maybe_wheel| {
            let when = scheduler::Handle::current().driver().time().time_source().instant_to_tick(this.deadline);
            if let Some((wheel, tx)) = maybe_wheel {
                let hdl = entry::new(when, cx.waker(), Some(tx));
                if unsafe { wheel.insert(hdl.clone()) } {
                    this.entry = Some(hdl);
                    // dbg!("Timer registered with deadline: {:?}", this.deadline);
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            } else {
                let hdl = entry::new(when, cx.waker(), None);
                push_inject(hdl);
                Poll::Pending
            }
        })
    }

    pub(crate) fn poll_elapsed(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        match self.entry.as_ref() {
            Some(entry) if entry.is_elapsed() => Poll::Ready(()),
            Some(entry) => {
                entry.register_waker(cx.waker());
                Poll::Pending
            }
            None => self.register(cx),
        }
    }

    pub(crate) fn cancel(self: Pin<&mut Self>) {
        if let Some(entry) = self.get_mut().entry.take() {
            entry.cancel();
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

fn instant_to_tick(instant: Instant) -> u64 {
    crate::runtime::scheduler::Handle::current().driver().time().time_source().instant_to_tick(instant)
}
