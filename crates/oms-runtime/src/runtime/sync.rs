//! Single-core, cooperative lock adapter for the Native OMS configuration.
//!
//! Native currently has no SMP or preemptive Process execution. `RefCell`'s
//! runtime borrow checks detect reentrant conflicts without unsafe code.

use core::cell::{BorrowError, BorrowMutError, Ref, RefCell, RefMut};

#[derive(Debug)]
pub struct RwLock<T>(RefCell<T>);

impl<T> RwLock<T> {
    pub const fn new(value: T) -> Self {
        Self(RefCell::new(value))
    }

    pub fn read(&self) -> Result<Ref<'_, T>, BorrowError> {
        self.0.try_borrow()
    }

    pub fn write(&self) -> Result<RefMut<'_, T>, BorrowMutError> {
        self.0.try_borrow_mut()
    }
}

pub type RwLockReadGuard<'a, T> = Ref<'a, T>;
pub type RwLockWriteGuard<'a, T> = RefMut<'a, T>;
