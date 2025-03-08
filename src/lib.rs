pub mod board_driver;
pub mod dsp;
pub mod eeg_system;

// Re-export the main types that users need
pub use eeg_system::EegSystem;
pub use board_driver::types::{AdcConfig, DriverType, DriverStatus};

/// Processed EEG data structure
#[derive(Clone, Debug)]
pub struct ProcessedData {
    pub data: Vec<Vec<f32>>,
    pub timestamp: u64,
    pub channel_count: usize,
}

// Optionally expose lower-level access through a raw module
pub mod raw {
    pub use crate::board_driver::*;
    pub use crate::dsp::*;
}