#![allow(dead_code)]

use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard};

pub(crate) mod agentic_cli;

pub(crate) struct ScopedEnvVar {
    key: &'static str,
    previous: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl ScopedEnvVar {
    pub(crate) fn set(
        lock: &'static Mutex<()>,
        key: &'static str,
        value: impl AsRef<OsStr>,
    ) -> Self {
        let lock = lock.lock().unwrap_or_else(|error| error.into_inner());
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self {
            key,
            previous,
            _lock: lock,
        }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}
