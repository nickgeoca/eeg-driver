use std::time::SystemTime;
use std::error::Error;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tokio::sync::mpsc;
use std::future::Future;
use std::pin::Pin;
use async_trait::async_trait;
use super::mock_driver::MockDriver;

// Driver events
#[derive(Debug, Clone)]
pub enum DriverEvent {
    Data(Vec<AdcData>),
    Error(String),
    StatusChange(DriverStatus),
}

// Driver status
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DriverStatus {
    NotInitialized,
    Ok,
    Error,
    Stopped,
    Running,
}

// ADC configuration
#[derive(Clone, Debug)]
pub struct AdcConfig {
    pub sample_rate: u32,
    pub gain: f32,
    pub channels: Vec<usize>,
    pub mock: bool,
    // Add other configuration parameters as needed
}

// ADC data point
#[derive(Clone, Debug)]
pub struct AdcData {
    pub samples: Vec<Vec<f32>>,
    pub timestamp: u64,
}

// Driver error
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("Hardware not found")]
    HardwareNotFound(String),
    
    #[error("Configuration error: {0}")]
    ConfigurationError(String),
    
    #[error("Acquisition error: {0}")]
    AcquisitionError(String),
    
    #[error("Driver not initialized")]
    NotInitialized,
    
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    
    #[error("Other error: {0}")]
    Other(String),
    
    #[error("Driver not configured")]
    NotConfigured,
}

// Remove the problematic From implementations that violate orphan rules
// Instead, create wrapper types for external errors
#[derive(Debug)]
pub struct SpiError(rppal::spi::Error);

#[derive(Debug)]
pub struct TimeError(std::time::SystemTimeError);

impl From<SpiError> for DriverError {
    fn from(err: SpiError) -> Self {
        DriverError::Other(err.0.to_string())
    }
}

impl From<TimeError> for DriverError {
    fn from(err: TimeError) -> Self {
        DriverError::Other(err.0.to_string())
    }
}

// Fix DriverType enum to match create_driver usage
#[derive(Debug, Clone, Copy)]
pub enum DriverType {
    // Ads1299,
    Mock,
}


#[async_trait]
pub trait AdcDriver: Send + Sync + 'static {
    async fn reset_and_start(&mut self, config: AdcConfig) -> Result<(), DriverError>;
    async fn shutdown(&mut self) -> Result<(), DriverError>;

    async fn start_acquisition(&mut self) -> Result<(), DriverError>;
    async fn stop_acquisition(&mut self) -> Result<(), DriverError>;

    fn get_status(&self) -> DriverStatus;
    fn get_config(&self) -> Result<AdcConfig, DriverError>;
}

// Updated factory function to create the appropriate driver and return the event channel
pub async fn create_driver(driver_type: DriverType, config: AdcConfig) 
    -> Result<(Box<dyn AdcDriver>, mpsc::Receiver<DriverEvent>), DriverError> {
    
    match driver_type {
        // DriverType::Ads1299 => {
        //     // Create the ADS1299 hardware driver
        //     let (driver, events) = crate::adc::ads1299_driver::Ads1299Driver::new(config).map_err(|e| DriverError::Other(e.to_string()))?;
            
        //     // Check if the driver is in error state after creation
        //     if driver.get_status() == DriverStatus::Error {
        //         return Err(DriverError::HardwareNotFound("Failed to initialize ADS1299 hardware".to_string()));
        //     }
            
        //     Ok((Box::new(driver), events))
        // },
        DriverType::Mock => {
            let (mut driver, events) = super::mock_driver::MockDriver::new()?;
            Ok((Box::new(driver), events))
        }
    }
}
