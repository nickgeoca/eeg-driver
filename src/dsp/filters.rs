use biquad::{Biquad, DirectForm2Transposed, Coefficients, Type, Q_BUTTERWORTH_F32, ToHertz};
use rustfft::{FftPlanner, num_complex::Complex};
use std::sync::Arc;
use std::time::Instant;
use std::f32::consts::PI;
// TODO add ADS1299 constants
use std::error::Error;
use std::thread::sleep;
use std::time::{Duration, SystemTime};
use tokio;
use crate::board_driver::types::DriverError;
use crate::board_driver::types::AdcDriver;
use crate::board_driver::types::DriverStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

#[derive(Debug)]
pub struct FrequencyBins {
    // Delta (0.5-4 Hz) - 7 bins
    delta: Vec<f32>,  
    // Theta (4-8 Hz) - 8 bins
    theta: Vec<f32>,  
    // Alpha (8-13 Hz) - 10 bins
    alpha: Vec<f32>,  
    // Beta (13-30 Hz) - 17 bins
    beta: Vec<f32>,   
    // Gamma (30-150 Hz) - 32 bins
    gamma: Vec<f32>,
    line_noise_50hz: f32,  // Around 50Hz
    line_noise_60hz: f32,  // Around 60Hz
}

// Update the FilterCoefficients to use biquad's Coefficients
#[derive(Clone, Debug)]
struct FilterCoefficients {
    coeffs: Coefficients<f32>
}

// Simplify DigitalFilter to use biquad's DirectForm2Transposed
#[derive(Debug)]
struct DigitalFilter {
    filter: DirectForm2Transposed<f32>
}

impl DigitalFilter {
    fn new(coeffs: FilterCoefficients) -> Self {
        Self {
            filter: DirectForm2Transposed::new(coeffs.coeffs)
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        // Clamp input to prevent extreme values
        let x = x.clamp(-8192.0, 8191.0);
        let y = self.filter.run(x);
        // Clamp output to prevent instability
        y.clamp(-8192.0, 8191.0)
    }
}

// Add struct definitions for each filter type
#[derive(Debug)]
struct NotchFilter(DigitalFilter);

#[derive(Debug)]
struct HighpassFilter(DigitalFilter);

#[derive(Debug)]
struct LowpassFilter(DigitalFilter);

// Update NotchFilter implementation to use BandPass instead of NotchFilter
impl NotchFilter {
    fn new(sample_rate: f32, notch_freq: f32) -> Self {
        let q_factor = 30.0;  // High Q for narrow notch
        let coeffs = Coefficients::<f32>::from_params(
            Type::Notch,  // <-- Changed from BandPass to Notch
            sample_rate.hz(),
            notch_freq.hz(),
            q_factor
        ).unwrap();
        
        NotchFilter(DigitalFilter::new(FilterCoefficients { coeffs }))
    }
    
    fn process(&mut self, x: f32) -> f32 {
        self.0.process(x)
    }
}

// Similar updates for HighpassFilter
impl HighpassFilter {
    fn new(sample_rate: f32) -> Self {
        // Lower the cutoff frequency to reduce impact on low frequency signals
        let cutoff_freq = 0.1; // Changed from 0.5 to 0.1 Hz
        let coeffs = Coefficients::<f32>::from_params(
            Type::HighPass,
            sample_rate.hz(),
            cutoff_freq.hz(),
            Q_BUTTERWORTH_F32
        ).unwrap();
        
        Self(DigitalFilter::new(FilterCoefficients { coeffs }))
    }
    
    fn process(&mut self, x: f32) -> f32 {
        self.0.process(x)
    }
}

// And LowpassFilter
impl LowpassFilter {
    fn new(sample_rate: f32) -> Self {
        let cutoff_freq = 100.0;
        let coeffs = Coefficients::<f32>::from_params(
            Type::LowPass,
            sample_rate.hz(),
            cutoff_freq.hz(),
            Q_BUTTERWORTH_F32
        ).unwrap();
        
        Self(DigitalFilter::new(FilterCoefficients { coeffs }))
    }
    
    fn process(&mut self, x: f32) -> f32 {
        self.0.process(x)
    }
}

pub struct SignalProcessor {
    sample_rate: u32,
    num_channels: usize,
    notch_filters_50hz: Vec<NotchFilter>,
    notch_filters_60hz: Vec<NotchFilter>,
    highpass_filters: Vec<HighpassFilter>,
    lowpass_filters: Vec<LowpassFilter>,
}

impl SignalProcessor {
    pub fn new(sample_rate: u32, num_channels: usize) -> Self {
        // Add validation for sample rate
        assert!(sample_rate > 0, "Sample rate must be positive");
        assert!(sample_rate >= 200, "Sample rate should be at least 200Hz for proper filter operation");
        
        let sample_rate_f32 = sample_rate as f32;
        
        Self {
            sample_rate,
            num_channels,
            notch_filters_50hz: (0..num_channels)
                .map(|_| NotchFilter::new(sample_rate_f32, 50.0))
                .collect(),
            notch_filters_60hz: (0..num_channels)
                .map(|_| NotchFilter::new(sample_rate_f32, 60.0))
                .collect(),
            highpass_filters: (0..num_channels)
                .map(|_| HighpassFilter::new(sample_rate_f32))
                .collect(),
            lowpass_filters: (0..num_channels)
                .map(|_| LowpassFilter::new(sample_rate_f32))
                .collect(),
        }
    }

    pub fn process_sample(&mut self, channel: usize, sample: f32) -> f32 {
        // Add channel bounds check
        assert!(channel < self.num_channels, "Channel index out of bounds");
        
        let mut processed = sample;
        processed = self.highpass_filters[channel].process(processed);  // Move highpass first
        processed = self.notch_filters_50hz[channel].process(processed);
        processed = self.notch_filters_60hz[channel].process(processed);
        processed = self.lowpass_filters[channel].process(processed);
        processed
    }

    pub fn reset(&mut self, new_sample_rate: u32, new_num_channels: usize) {
        self.sample_rate = new_sample_rate;
        self.num_channels = new_num_channels;
        // Recreate all filters
        let sample_rate = self.sample_rate as f32;
        self.notch_filters_50hz = (0..self.num_channels)
            .map(|_| NotchFilter::new(sample_rate, 50.0))
            .collect();
        self.notch_filters_60hz = (0..self.num_channels)
            .map(|_| NotchFilter::new(sample_rate, 60.0))
            .collect();
        self.highpass_filters = (0..self.num_channels)
            .map(|_| HighpassFilter::new(sample_rate))
            .collect();
        self.lowpass_filters = (0..self.num_channels)
            .map(|_| LowpassFilter::new(sample_rate))
            .collect();
    }
}