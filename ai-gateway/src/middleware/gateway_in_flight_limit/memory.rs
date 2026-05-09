//! In-process whole-gateway in-flight counter.

use std::sync::Mutex;

pub struct MemoryInFlightLimiter {
    max: u32,
    inner: Mutex<u32>,
}

impl MemoryInFlightLimiter {
    #[must_use]
    pub fn new(max: u32) -> Self {
        Self {
            max,
            inner: Mutex::new(0),
        }
    }

    /// `Ok(())` slot taken; `Err(())` rejected.
    pub fn try_acquire(&self) -> Result<(), ()> {
        if self.max == 0 {
            return Ok(());
        }
        let mut g = self.inner.lock().expect("memory in-flight mutex poisoned");
        if *g >= self.max {
            return Err(());
        }
        *g += 1;
        Ok(())
    }

    pub fn release(&self) {
        let mut g = self.inner.lock().expect("memory in-flight mutex poisoned");
        *g = (*g).saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn third_acquire_fails_until_release() {
        let lim = MemoryInFlightLimiter::new(2);
        assert!(lim.try_acquire().is_ok());
        assert!(lim.try_acquire().is_ok());
        assert!(lim.try_acquire().is_err());
        lim.release();
        assert!(lim.try_acquire().is_ok());
    }
}
