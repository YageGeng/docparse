//! Cancellable deadlines without requiring a Tokio timer inside browser Workers.
use std::future::Future;
use std::time::Duration;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;

    /// Uses the native runtime timer and drops the operation when its deadline expires.
    pub(crate) async fn timeout<F: Future>(
        duration: Duration,
        future: F,
    ) -> Result<F::Output, ()> {
        tokio::time::timeout(duration, future)
            .await
            .map_err(|_elapsed| ())
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod platform {
    use super::*;
    use futures_util::future::{Either, select};
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::sync::oneshot;
    use wasm_bindgen::{JsCast, prelude::*};

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name = setTimeout)]
        fn set_timeout(callback: &js_sys::Function, millis: f64) -> i32;
        #[wasm_bindgen(js_name = clearTimeout)]
        fn clear_timeout(id: i32);
    }

    /// Owns its JS callback so completion or cancellation releases both timer and closure.
    struct Deadline {
        id: i32,
        _callback: Closure<dyn FnMut()>,
        receiver: oneshot::Receiver<()>,
    }

    impl Deadline {
        /// Starts a Worker timer whose callback is canceled on drop.
        fn new(duration: Duration) -> Self {
            let (sender, receiver) = oneshot::channel();
            let mut sender = Some(sender);
            let callback = Closure::wrap(Box::new(move || {
                if let Some(sender) = sender.take() {
                    let _ = sender.send(());
                }
            }) as Box<dyn FnMut()>);
            let id = set_timeout(
                callback.as_ref().unchecked_ref(),
                duration.as_secs_f64() * 1000.0,
            );
            Self {
                id,
                _callback: callback,
                receiver,
            }
        }
    }

    impl Future for Deadline {
        type Output = ();
        /// Waits for the owned callback without borrowing browser linear-memory buffers.
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            Pin::new(&mut self.receiver).poll(cx).map(|_| ())
        }
    }

    impl Drop for Deadline {
        /// Cancels callbacks even when the operation wins the race or the parser closes.
        fn drop(&mut self) {
            clear_timeout(self.id);
        }
    }

    /// Races a local future against an owned Worker timer and releases the losing branch.
    pub(crate) async fn timeout<F: Future>(
        duration: Duration,
        future: F,
    ) -> Result<F::Output, ()> {
        match select(Box::pin(future), Box::pin(Deadline::new(duration))).await
        {
            Either::Left((value, _)) => Ok(value),
            Either::Right(_) => Err(()),
        }
    }
}

pub(crate) use platform::timeout;
