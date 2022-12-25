use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{
            AtomicU8,
            Ordering::{Acquire, Relaxed},
        },
        Arc,
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, Thread},
};

const EMPTY: u8 = 0;
const WAITING: u8 = 1;
const NOTIFIED: u8 = 2;

struct Poller {
    flag: AtomicU8,
    thread: Thread,
}

impl Poller {
    fn new() -> Self {
        Self {
            flag: AtomicU8::new(EMPTY),
            thread: thread::current(),
        }
    }

    fn wait(&self) {
        match self
            .flag
            .compare_exchange(EMPTY, WAITING, Acquire, Acquire)
            .unwrap_or_else(|x| x)
        {
            EMPTY => {
                while self.flag.load(Acquire) == WAITING {
                    thread::park();
                }
            }
            NOTIFIED => {
                self.flag.store(EMPTY, Relaxed);
            }
            _ => (),
        }
    }

    fn notify(&self) {
        match self
            .flag
            .compare_exchange(EMPTY, NOTIFIED, Acquire, Acquire)
            .unwrap_or_else(|x| x)
        {
            WAITING => {
                self.flag.store(EMPTY, Relaxed);
                self.thread.unpark();
            }
            _ => (),
        }
    }
}

impl Wake for Poller {
    fn wake(self: Arc<Self>) {
        self.notify();
    }
}

pub fn block_on<F: Future>(mut fut: F) -> <F as Future>::Output {
    let mut fut = unsafe { Pin::new_unchecked(&mut fut) };
    let signal = Arc::new(Poller::new());

    let waker = Waker::from(Arc::clone(&signal));
    let mut context = Context::from_waker(&waker);

    loop {
        match fut.as_mut().poll(&mut context) {
            Poll::Pending => signal.wait(),
            Poll::Ready(val) => return val,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn basic() {
        let make_fut = || std::future::ready(42);

        // Immediately ready
        assert_eq!(block_on(make_fut()), 42);

        // Ready after a timeout
        let then = Instant::now();
        block_on(futures_timer::Delay::new(Duration::from_millis(250)));
        assert!(Instant::now().duration_since(then) > Duration::from_millis(250));
    }

    #[test]
    fn mpsc() {
        use std::{
            sync::atomic::{AtomicUsize, Ordering::SeqCst},
            thread,
        };
        use tokio::sync::mpsc;

        const BOUNDED: usize = 16;
        const MESSAGES: usize = 100_000;

        let (a_tx, mut a_rx) = mpsc::channel(BOUNDED);
        let (b_tx, mut b_rx) = mpsc::channel(BOUNDED);

        let thread_a = thread::spawn(move || {
            block_on(async {
                while let Some(msg) = a_rx.recv().await {
                    b_tx.send(msg).await.expect("send on b");
                }
            });
        });

        let thread_b = thread::spawn(move || {
            block_on(async move {
                for _ in 0..MESSAGES {
                    a_tx.send(()).await.expect("Send on a");
                }
            });
        });

        block_on(async move {
            let sum = AtomicUsize::new(0);

            while sum.fetch_add(1, SeqCst) < MESSAGES {
                b_rx.recv().await;
            }

            assert_eq!(sum.load(SeqCst), MESSAGES + 1);
        });

        thread_a.join().expect("join thread_a");
        thread_b.join().expect("join thread_b");
    }
}
