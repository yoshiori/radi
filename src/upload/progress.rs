//! Shared progress state for the LISTEN upload pipeline.
//!
//! The upload thread writes here; the render thread reads during each TUI
//! tick. All fields are atomic so progress can flow without locks — same
//! pattern as the audio peak meter.
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadPhase {
    Preparing = 0,
    Uploading = 1,
    Finalizing = 2,
}

#[derive(Debug, Default)]
pub struct UploadProgress {
    phase: AtomicU8,
    bytes_uploaded: AtomicU64,
    bytes_total: AtomicU64,
}

impl UploadProgress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set_phase(&self, p: UploadPhase) {
        self.phase.store(p as u8, Ordering::Relaxed);
    }

    pub fn phase(&self) -> UploadPhase {
        match self.phase.load(Ordering::Relaxed) {
            0 => UploadPhase::Preparing,
            1 => UploadPhase::Uploading,
            _ => UploadPhase::Finalizing,
        }
    }

    pub fn set_total(&self, n: u64) {
        self.bytes_total.store(n, Ordering::Relaxed);
    }

    pub fn total(&self) -> u64 {
        self.bytes_total.load(Ordering::Relaxed)
    }

    pub fn add_uploaded(&self, n: u64) {
        self.bytes_uploaded.fetch_add(n, Ordering::Relaxed);
    }

    pub fn uploaded(&self) -> u64 {
        self.bytes_uploaded.load(Ordering::Relaxed)
    }

    /// 0.0..=1.0; `None` when `bytes_total` has not been set yet.
    pub fn ratio(&self) -> Option<f64> {
        let t = self.total();
        if t == 0 {
            return None;
        }
        Some((self.uploaded() as f64 / t as f64).clamp(0.0, 1.0))
    }
}

/// `Read` wrapper that records bytes flowing through it into an
/// `UploadProgress`. Used to observe the PUT body from the upload thread
/// without changing how reqwest streams the request.
pub struct ProgressReader<R: Read> {
    inner: R,
    progress: Arc<UploadProgress>,
}

impl<R: Read> ProgressReader<R> {
    pub fn new(inner: R, progress: Arc<UploadProgress>) -> Self {
        Self { inner, progress }
    }
}

impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.progress.add_uploaded(n as u64);
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, ErrorKind, Read};

    #[test]
    fn progress_starts_at_zero_and_preparing() {
        let p = UploadProgress::new();
        assert_eq!(p.uploaded(), 0);
        assert_eq!(p.total(), 0);
        assert_eq!(p.phase(), UploadPhase::Preparing);
        assert!(p.ratio().is_none());
    }

    #[test]
    fn add_uploaded_accumulates() {
        let p = UploadProgress::new();
        p.add_uploaded(10);
        p.add_uploaded(5);
        assert_eq!(p.uploaded(), 15);
    }

    #[test]
    fn ratio_is_none_when_total_unset() {
        let p = UploadProgress::new();
        p.add_uploaded(100);
        assert!(p.ratio().is_none());
    }

    #[test]
    fn ratio_is_clamped_to_one() {
        let p = UploadProgress::new();
        p.set_total(10);
        p.add_uploaded(25);
        assert_eq!(p.ratio(), Some(1.0));
    }

    #[test]
    fn phase_round_trip() {
        let p = UploadProgress::new();
        for phase in [
            UploadPhase::Preparing,
            UploadPhase::Uploading,
            UploadPhase::Finalizing,
        ] {
            p.set_phase(phase);
            assert_eq!(p.phase(), phase);
        }
    }

    #[test]
    fn progress_reader_counts_bytes_read() {
        let p = UploadProgress::new();
        let mut r = ProgressReader::new(Cursor::new(b"abcdefgh".to_vec()), p.clone());
        let mut buf = [0u8; 3];
        let n1 = r.read(&mut buf).unwrap();
        let n2 = r.read(&mut buf).unwrap();
        assert_eq!(n1, 3);
        assert_eq!(n2, 3);
        assert_eq!(p.uploaded(), 6);
        // Drain the last 2 bytes.
        let n3 = r.read(&mut buf).unwrap();
        assert_eq!(n3, 2);
        assert_eq!(p.uploaded(), 8);
    }

    #[test]
    fn progress_reader_zero_read_does_not_increment() {
        struct EofReader;
        impl Read for EofReader {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Ok(0)
            }
        }
        let p = UploadProgress::new();
        let mut r = ProgressReader::new(EofReader, p.clone());
        let mut buf = [0u8; 8];
        assert_eq!(r.read(&mut buf).unwrap(), 0);
        assert_eq!(p.uploaded(), 0);
    }

    #[test]
    fn progress_reader_propagates_errors() {
        struct ErrReader;
        impl Read for ErrReader {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(ErrorKind::Interrupted, "boom"))
            }
        }
        let p = UploadProgress::new();
        let mut r = ProgressReader::new(ErrReader, p.clone());
        let mut buf = [0u8; 8];
        let err = r.read(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Interrupted);
        assert_eq!(p.uploaded(), 0);
    }
}
