use std::cell::RefCell;

pub(crate) const BLOCKING_IO_BUFFER_BYTES: usize = 1024 * 1024;

thread_local! {
    static BLOCKING_IO_BUFFER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Reuses one sequential-I/O buffer per blocking worker thread. Nested use is
/// rare, but falls back to a temporary buffer instead of panicking on a
/// `RefCell` borrow conflict.
pub(crate) fn with_blocking_io_buffer<T>(f: impl FnOnce(&mut [u8]) -> T) -> T {
    BLOCKING_IO_BUFFER.with(|buffer| {
        if let Ok(mut buffer) = buffer.try_borrow_mut() {
            if buffer.len() != BLOCKING_IO_BUFFER_BYTES {
                buffer.resize(BLOCKING_IO_BUFFER_BYTES, 0);
            }
            return f(buffer.as_mut_slice());
        }

        let mut fallback = vec![0u8; BLOCKING_IO_BUFFER_BYTES];
        f(&mut fallback)
    })
}

#[cfg(test)]
mod tests {
    use super::{with_blocking_io_buffer, BLOCKING_IO_BUFFER_BYTES};

    #[test]
    fn nested_use_falls_back_without_panicking() {
        with_blocking_io_buffer(|outer| {
            assert_eq!(outer.len(), BLOCKING_IO_BUFFER_BYTES);
            outer[0] = 1;
            with_blocking_io_buffer(|inner| {
                assert_eq!(inner.len(), BLOCKING_IO_BUFFER_BYTES);
                inner[0] = 2;
            });
            assert_eq!(outer[0], 1);
        });
    }
}
