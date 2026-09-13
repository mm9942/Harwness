//! Deterministic backpressure policy for the normalized BiDi event journal.

use harw_browser::error::Error as BrowserError;
use std::num::NonZeroUsize;

/// Event classes with distinct retention and pressure characteristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BidiEventClass {
    CriticalControl,
    RequestLifecycle,
    Console,
    StaticAsset,
    ArtifactPayload,
}

/// Required behavior when an event class encounters journal pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackpressureDisposition {
    Retain,
    AggregateWhenFull,
    Deduplicate,
    Sample,
    StoreExternally,
}

/// Capacity and deterministic per-class pressure behavior for an event pump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventJournalPolicy {
    capacity: NonZeroUsize,
}

impl EventJournalPolicy {
    /// Creates a bounded policy. A zero-capacity journal cannot retain its
    /// critical control stream and is therefore rejected at configuration
    /// time rather than degrading silently.
    pub fn bounded(capacity: usize) -> harw_browser::Result<Self> {
        let Some(capacity) = NonZeroUsize::new(capacity) else {
            return Err(BrowserError::InvalidArgument {
                detail: "event journal capacity must be greater than zero".to_owned(),
            });
        };
        Ok(Self { capacity })
    }

    pub fn capacity(&self) -> usize {
        self.capacity.get()
    }

    pub fn disposition(&self, event_class: BidiEventClass) -> BackpressureDisposition {
        match event_class {
            BidiEventClass::CriticalControl => BackpressureDisposition::Retain,
            BidiEventClass::RequestLifecycle => BackpressureDisposition::AggregateWhenFull,
            BidiEventClass::Console => BackpressureDisposition::Deduplicate,
            BidiEventClass::StaticAsset => BackpressureDisposition::Sample,
            BidiEventClass::ArtifactPayload => BackpressureDisposition::StoreExternally,
        }
    }
}
