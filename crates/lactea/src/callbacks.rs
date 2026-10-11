//! Host callbacks (`callbacks.h`): `projectm_set_preset_switch_requested_event_callback` and
//! `projectm_set_preset_switch_failed_event_callback`.
//!
//! A [`Hook`] is an optional, cheaply clonable `Fn` that the owner invokes synchronously from the
//! call that raises the event (the same threading contract as projectM: the callback runs on the
//! caller's thread, inside the call).

use std::fmt;
use std::sync::Arc;

/// An optional callback taking `&A`. Clones share the same underlying callback.
pub struct Hook<A>(Option<Callback<A>>);

type Callback<A> = Arc<dyn Fn(&A) + Send + Sync>;

impl<A> Hook<A> {
    /// A hook with no callback installed.
    pub const fn none() -> Self {
        Self(None)
    }

    /// Install `f`, or remove the callback with `None`.
    pub fn set(&mut self, f: Option<impl Fn(&A) + Send + Sync + 'static>) {
        self.0 = f.map(|f| Arc::new(f) as Callback<A>);
    }

    /// Whether a callback is installed.
    pub fn is_set(&self) -> bool {
        self.0.is_some()
    }

    /// Call the callback, if any.
    pub fn fire(&self, arg: &A) {
        if let Some(f) = &self.0 {
            f(arg);
        }
    }
}

impl<A> Default for Hook<A> {
    fn default() -> Self {
        Self::none()
    }
}

impl<A> Clone for Hook<A> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<A> fmt::Debug for Hook<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "Hook(set)"
        } else {
            "Hook(none)"
        })
    }
}

/// A failed preset load (`projectm_preset_switch_failed_event`): the preset's name and the
/// error text. The previously loaded preset keeps running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchFailure {
    /// Name of the preset that failed to load.
    pub name: String,
    /// Error text of the failed load.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn hook_fires_until_removed() {
        let n = Arc::new(AtomicU32::new(0));
        let mut h = Hook::<u32>::none();
        h.fire(&1);
        let n2 = n.clone();
        h.set(Some(move |v: &u32| {
            n2.fetch_add(*v, Ordering::SeqCst);
        }));
        let copy = h.clone();
        h.fire(&2);
        copy.fire(&3);
        assert_eq!(n.load(Ordering::SeqCst), 5);
        h.set(None::<fn(&u32)>);
        h.fire(&10);
        assert_eq!(n.load(Ordering::SeqCst), 5);
    }
}
