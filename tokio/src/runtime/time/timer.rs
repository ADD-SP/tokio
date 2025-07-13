use super::wheel::EntryHandle;
use crate::{
    runtime::{
        context,
        time::{
            wheel::{entry, MAX_SAFE_MILLIS_DURATION},
            Wheel,
        },
    },
    time::Instant,
    util::error::RUNTIME_SHUTTING_DOWN_ERROR,
};
use std::{
    pin::Pin,
    sync::mpsc,
    task::{Context, Poll},
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
        eprintln!("cancelled timer: {:?}", self.deadline);
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
        self.entry
            .as_ref()
            .map_or(false, |entry| entry.is_elapsed())
    }

    fn register(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();

        with_current_wheel(|maybe_wheel| {
            let when = deadline_to_tick(this.deadline);
            assert!(
                when <= MAX_SAFE_MILLIS_DURATION,
                "Timer deadline exceeds maximum safe duration"
            );
            if let Some((wheel, tx)) = maybe_wheel {
                let hdl = entry::new(when, cx.waker(), Some(tx));
                if unsafe { wheel.insert(hdl.clone()) } {
                    this.entry = Some(hdl);
                    eprintln!("timer registered");
                    Poll::Pending
                } else {
                    eprintln!("timer is already expired, then fired immediately");
                    Poll::Ready(())
                }
            } else {
                let hdl = entry::new(when, cx.waker(), None);
                this.entry = Some(hdl.clone());
                eprintln!("timer push in inject");
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
    })
    .unwrap();
}

fn deadline_to_tick(deadline: Instant) -> u64 {
    let binding = crate::runtime::scheduler::Handle::current();
    let hdl = binding.driver().time();

    if hdl.is_shutdown() {
        panic!("{RUNTIME_SHUTTING_DOWN_ERROR}");
    }

    hdl.time_source().deadline_to_tick(deadline)
}
