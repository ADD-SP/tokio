cfg_rt_and_time! {
    pub(crate) mod time {
        use crate::runtime::{scheduler::driver};
        use crate::runtime::time::Context2;
        use crate::runtime::time::EntryHandle;
        use crate::util::WakeList;
        use std::time::Duration;

        fn with_context2<F, R>(f: F) -> R
        where
            F: FnOnce(&mut Context2) -> R,
        {
            crate::runtime::context::with_scheduler(|maybe_sched_cx| {
                let scheduler_cx = maybe_sched_cx.expect("should be called inside a runtime");
                match scheduler_cx {
                    crate::runtime::scheduler::Context::CurrentThread(cx) => {
                        let mut maybe_core = cx.core.borrow_mut();
                        let core = maybe_core.as_mut().expect("core should be present").as_mut();
                        f(&mut core.time_context)
                    }
                    #[cfg(feature = "rt-multi-thread")]
                    crate::runtime::scheduler::Context::MultiThread(cx) => {
                        let mut maybe_core = cx.core.borrow_mut();
                        let core = maybe_core.as_mut().expect("core should be present").as_mut();
                        f(&mut core.time_context)
                    }
                }
            })
        }

        fn process_at_time(mut now: u64) {
            loop {
                let mut waker_list = with_context2(|time_context| {
                    let wheel = &mut time_context.wheel;
                    if now < wheel.elapsed() {
                        // Time went backwards! This normally shouldn't happen as the Rust language
                        // guarantees that an Instant is monotonic, but can happen when running
                        // Linux in a VM on a Windows host due to std incorrectly trusting the
                        // hardware clock to be monotonic.
                        //
                        // See <https://github.com/tokio-rs/tokio/issues/3619> for more information.
                        now = wheel.elapsed();
                    }

                    let mut waker_list = WakeList::new();
                    while let Some(hdl) = wheel.poll(now) {
                        match hdl.take_waker() {
                            Some(waker) if waker_list.can_push() => {
                                waker_list.push(waker);
                            }
                            Some(_waker) => unreachable!("waker list is full"),
                            None => {}
                        }

                        if !waker_list.can_push() {
                            break;
                        }
                    }
                    waker_list
                });

                if waker_list.is_empty() {
                    break;
                }
                waker_list.wake_all();
            }
        }

        pub(crate) fn insert_inject_timers(
            mut inject: Vec<EntryHandle>,
        ) -> bool {
            use crate::runtime::time::Insert;

            let mut fired = false;
            loop {
                let mut waker_list = with_context2(|time_context| {
                    let mut waker_list = WakeList::new();
                    let wheel = &mut time_context.wheel;
                    let canc_tx = &time_context.canc_tx;
                    while let Some(hdl) = inject.pop() {
                        match unsafe { wheel.insert(hdl.clone(), canc_tx.clone()) } {
                            Insert::Success => {}
                            Insert::Elapsed => {
                                let waker = hdl.take_waker_unregistered();
                                match waker {
                                    Some(waker) if waker_list.can_push() => {
                                        waker_list.push(waker);
                                    }
                                    Some(_waker) => break,
                                    None => {}
                                }
                            }
                            Insert::Cancelling => {}
                        }
                    }
                    waker_list
                });

                if waker_list.is_empty() {
                    break;
                }

                waker_list.wake_all();
                fired = true;
            }

            fired
        }

        pub(crate) fn remove_cancelled_timers() {
            with_context2(|time_context| {
                for hdl in time_context.canc_rx.recv_all() {
                    let is_registered = hdl.is_registered();
                    let is_pending = hdl.is_pending();
                    if is_registered && !is_pending {
                        unsafe {
                            time_context.wheel.remove(hdl);
                        }
                    }
                }
            });
        }

        pub(crate) fn next_expiration_time(
            drv_hdl: &driver::Handle,
        ) -> Option<Duration> {
            drv_hdl.with_time(|maybe_time_hdl| {
                let Some(time_hdl) = maybe_time_hdl else {
                    // time driver is not enabled, nothing to do.
                    return None;
                };

                let clock = drv_hdl.clock();
                let time_source = time_hdl.time_source();

                with_context2(|time_context| {
                    time_context.wheel.next_expiration_time().map(|tick| {
                        let now = time_source.now(clock);
                        time_source.tick_to_duration(tick.saturating_sub(now))
                    })
                })
            })
        }

        cfg_test_util! {
            pub(crate) fn pre_auto_advance(
                drv_hdl: &driver::Handle,
                duration: Option<Duration>,
            ) -> bool {
                drv_hdl.with_time(|maybe_time_hdl| {
                    if maybe_time_hdl.is_none() {
                        // time driver is not enabled, nothing to do.
                        return false;
                    }

                    if duration.is_some() {
                        let clock = drv_hdl.clock();
                        if clock.can_auto_advance() {
                            return true;
                        }

                        false
                    } else {
                        false
                    }
                })
            }

            pub(crate) fn post_auto_advance(
                drv_hdl: &driver::Handle,
                duration: Option<Duration>,
            ) {
                drv_hdl.with_time(|maybe_time_hdl| {
                    let Some(time_hdl) = maybe_time_hdl else {
                        // time driver is not enabled, nothing to do.
                        return;
                    };

                    if let Some(park_duration) = duration {
                        let clock = drv_hdl.clock();
                        if clock.can_auto_advance()
                            && !time_hdl.did_wake() {
                                if let Err(msg) = clock.advance(park_duration) {
                                    panic!("{msg}");
                                }
                            }
                    }
                })
            }
        }

        cfg_not_test_util! {
            pub(crate) fn pre_auto_advance(
                _drv_hdl: &driver::Handle,
                _duration: Option<Duration>,
            ) -> bool {
                false
            }

            pub(crate) fn post_auto_advance(
                _drv_hdl: &driver::Handle,
                _duration: Option<Duration>,
            ) {
                // No-op in non-test util builds
            }
        }

        pub(crate) fn process_expired_timers(
            drv_hdl: &driver::Handle,
        ) {
            drv_hdl.with_time(|maybe_time_hdl| {
                let Some(time_hdl) = maybe_time_hdl else {
                    // time driver is not enabled, nothing to do.
                    return;
                };

                let clock = drv_hdl.clock();
                let time_source = time_hdl.time_source();

                let now = time_source.now(clock);
                process_at_time(now);
            });
        }

        pub(crate) fn shutdown_local_timers(
            time_context: &mut Context2,
            inject: Vec<EntryHandle>,
            drv_hdl: &driver::Handle,
        ) {
            use crate::runtime::time::Insert;

            drv_hdl.with_time(|maybe_time_hdl| {
                if maybe_time_hdl.is_none() {
                    // time driver is not enabled, nothing to do.
                    return;
                }

                for hdl in time_context.canc_rx.recv_all() {
                    let is_registered = hdl.is_registered();
                    let is_pending = hdl.is_pending();
                    if is_registered && !is_pending {
                        unsafe {
                            time_context.wheel.remove(hdl);
                        }
                    }
                }


                for hdl in inject {
                    match unsafe { time_context.wheel.insert(hdl.clone(), time_context.canc_tx.clone()) } {
                        Insert::Success => {}
                        Insert::Elapsed => {
                            if let Some(waker) = hdl.take_waker_unregistered() {
                                waker.wake();
                            }
                        }
                        Insert::Cancelling => {}
                    }
                }

                const MAX_TICK: u64 = u64::MAX;
                while let Some(hdl) = time_context.wheel.poll(MAX_TICK) {
                    if let Some(waker) = hdl.take_waker() {
                        waker.wake();
                    }
                }
            });
        }
    }
}
