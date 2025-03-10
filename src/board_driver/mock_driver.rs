use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use async_trait::async_trait;
use log::{info, warn, debug, trace, error};
use super::types::{AdcConfig, AdcData, DriverStatus, DriverError, DriverEvent};

/// A stubbed-out driver that does not access any hardware.
pub struct MockDriver {
    inner: Arc<Mutex<MockInner>>,
    task_handle: Option<JoinHandle<()>>,
    tx: mpsc::Sender<DriverEvent>,
    additional_channel_buffering: usize,
}

/// Internal state for the MockDriver.
struct MockInner {
    config: Option<AdcConfig>,
    running: bool,
    status: DriverStatus,
}

/// Helper function to get current timestamp in microseconds
fn current_timestamp_micros() -> Result<u64, DriverError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .map_err(|e| DriverError::Other(format!("Failed to get timestamp: {}", e)))
}

impl MockDriver {
    /// Create a new instance of the MockDriver.
    ///
    /// This constructor takes an ADC configuration and an optional additional channel buffering parameter.
    /// The additional_channel_buffering parameter determines how many extra batches can be buffered in the channel
    /// beyond the minimum required (which is the batch_size from the config). Setting this to 0 minimizes
    /// latency but may cause backpressure if the consumer can't keep up.
    ///
    /// Returns a tuple containing the driver instance and a receiver for driver events.
    /// Create a new instance of the MockDriver.
    ///
    /// This constructor takes an ADC configuration and an optional additional channel buffering parameter.
    /// The additional_channel_buffering parameter determines how many extra batches can be buffered in the channel
    /// beyond the minimum required (which is the batch_size from the config). Setting this to 0 minimizes
    /// latency but may cause backpressure if the consumer can't keep up.
    ///
    /// # Important
    /// Users should explicitly call `shutdown()` when done with the driver to ensure proper cleanup.
    /// While the Drop implementation provides some basic cleanup, it cannot perform the full async shutdown sequence.
    ///
    /// # Returns
    /// A tuple containing the driver instance and a receiver for driver events.
    ///
    /// # Errors
    /// Returns an error if:
    /// - config.mock is false (MockDriver requires mock=true)
    /// - config.batch_size is 0 (batch size must be positive)
    /// - config.batch_size is less than the number of channels (need at least one sample per channel)
    pub fn new(
        config: AdcConfig,
        additional_channel_buffering: usize
    ) -> Result<(Self, mpsc::Receiver<DriverEvent>), DriverError> {
        // Validate config
        if !config.mock {
            return Err(DriverError::ConfigurationError(
                "MockDriver requires config.mock=true".to_string()
            ));
        }
        
        // Validate batch size
        if config.batch_size == 0 {
            return Err(DriverError::ConfigurationError(
                "Batch size must be greater than 0".to_string()
            ));
        }
        
        // Validate batch size relative to channel count
        if config.batch_size < config.channels.len() {
            return Err(DriverError::ConfigurationError(
                format!("Batch size ({}) must be at least equal to the number of channels ({})",
                        config.batch_size, config.channels.len())
            ));
        }
        
        // Validate total buffer size (prevent excessive memory usage)
        const MAX_BUFFER_SIZE: usize = 10000; // Arbitrary limit to prevent excessive memory usage
        let channel_buffer_size = config.batch_size + additional_channel_buffering;
        if channel_buffer_size > MAX_BUFFER_SIZE {
            return Err(DriverError::ConfigurationError(
                format!("Total buffer size ({}) exceeds maximum allowed ({})",
                        channel_buffer_size, MAX_BUFFER_SIZE)
            ));
        }
        
        let inner = MockInner {
            config: Some(config.clone()),
            running: false,
            status: DriverStatus::Ok,
        };
        
        // Create channel with validated buffer size
        let (tx, rx) = mpsc::channel(channel_buffer_size);
        
        let driver = MockDriver {
            inner: Arc::new(Mutex::new(inner)),
            task_handle: None,
            tx,
            additional_channel_buffering,
        };
        
        info!("MockDriver created with config: {:?}", config);
        info!("Channel buffer size: {} (batch_size: {} + additional_buffering: {})",
              channel_buffer_size, config.batch_size, additional_channel_buffering);
        
        Ok((driver, rx))
    }
    
    /// Return the current configuration.
    ///
    /// Returns an error if the driver has not been configured.
    pub(crate) fn get_config(&self) -> Result<AdcConfig, DriverError> {
        let inner = self.inner.lock().map_err(|_|
            DriverError::Other("Failed to acquire lock on driver state".to_string()))?;
        inner.config.clone().ok_or(DriverError::NotConfigured)
    }

    /// Start a dummy acquisition task that sends fake data at regular intervals.
    ///
    /// This method validates the driver state and spawns a background task that
    /// generates synthetic data according to the configured parameters.
    pub(crate) async fn start_acquisition(&mut self) -> Result<(), DriverError> {
        // Check preconditions without holding the lock for too long
        {
            let inner = self.inner.lock().map_err(|_|
                DriverError::Other("Failed to acquire lock on driver state".to_string()))?;
                
            if inner.running {
                return Err(DriverError::ConfigurationError("Acquisition already running".to_string()));
            }
            if inner.config.is_none() {
                return Err(DriverError::NotConfigured);
            }
        }
        
        // Update state to running
        {
            let mut inner = self.inner.lock().map_err(|_|
                DriverError::Other("Failed to acquire lock on driver state".to_string()))?;
            inner.running = true;
            inner.status = DriverStatus::Running;
        }
        
        // Notify about the status change
        self.notify_status_change().await?;

        // Prepare for background task
        let inner_arc = self.inner.clone();
        let tx = self.tx.clone();
        
        // Spawn a task that periodically sends dummy data
        let handle = tokio::spawn(async move {
            // Get configuration without holding the lock for the entire task
            let config = {
                let inner = inner_arc.lock().map_err(|e| {
                    error!("Failed to acquire lock: {:?}", e);
                    return;
                }).unwrap();
                
                match inner.config.clone() {
                    Some(cfg) => cfg,
                    None => {
                        error!("Configuration missing in acquisition task");
                        return;
                    }
                }
            };
            
            // Get batch size from config
            let batch_size = config.batch_size;
            
            // Get initial time as our zero reference
            let start_time = match current_timestamp_micros() {
                Ok(time) => time,
                Err(e) => {
                    error!("Failed to get start timestamp: {:?}", e);
                    return;
                }
            };
            
            debug!("Starting acquisition with batch size: {}, sample rate: {} Hz",
                   batch_size, config.sample_rate);
            
            // Main acquisition loop
            while let Ok(inner) = inner_arc.lock() {
                if !inner.running {
                    break;
                }
                
                // Get the latest config (in case it was reconfigured)
                let current_config = match inner.config.clone() {
                    Some(cfg) => cfg,
                    None => {
                        error!("Configuration missing during acquisition");
                        break;
                    }
                };
                
                // Get the current batch size (may have changed due to reconfiguration)
                let current_batch_size = current_config.batch_size;
                
                drop(inner); // Release the lock before time-consuming operations
                
                // Calculate timing parameters
                let mut batch = Vec::with_capacity(current_batch_size);
                let sample_interval = (1_000_000 / current_config.sample_rate) as u64; // microseconds between samples
                debug!("Sample interval: {} microseconds", sample_interval);
                
                // Get current timestamp relative to start time
                let base_timestamp = match current_timestamp_micros() {
                    Ok(time) => time.saturating_sub(start_time),
                    Err(e) => {
                        error!("Failed to get current timestamp: {:?}", e);
                        break;
                    }
                };
                
                // Generate a batch of samples with incrementing timestamps
                for i in 0..current_batch_size {
                    let relative_timestamp = base_timestamp + i as u64 * sample_interval;
                    trace!("Sample {}: relative_time={} microseconds", i, relative_timestamp);
                    let sample = test_data(&current_config, relative_timestamp);
                    batch.push(sample);
                }
                
                // Send the batch of data
                if let Err(e) = tx.send(DriverEvent::Data(batch)).await {
                    warn!("MockDriver event channel closed: {}", e);
                    break;
                }
                
                // Sleep for the time it would take to collect this batch via SPI
                let sleep_time = (1000 * current_batch_size as u64) / current_config.sample_rate as u64;
                debug!("Sleeping for {} ms before next batch", sleep_time);
                sleep(Duration::from_millis(sleep_time)).await;
            }
            
            debug!("Acquisition task terminated");
        });
        
        self.task_handle = Some(handle);
        info!("MockDriver acquisition started");
        Ok(())
    }

    /// Stop the dummy data acquisition.
    ///
    /// This method signals the acquisition task to stop, waits for it to complete,
    /// and updates the driver status.
    pub(crate) async fn stop_acquisition(&mut self) -> Result<(), DriverError> {
        // Signal the acquisition task to stop
        {
            let mut inner = self.inner.lock().map_err(|_|
                DriverError::Other("Failed to acquire lock on driver state".to_string()))?;
            
            if !inner.running {
                debug!("Stop acquisition called, but acquisition was not running");
                return Ok(());
            }
            
            inner.running = false;
            debug!("Signaled acquisition task to stop");
        }
        
        // Wait for the task to complete
        if let Some(handle) = self.task_handle.take() {
            match handle.await {
                Ok(_) => debug!("Acquisition task completed successfully"),
                Err(e) => warn!("Acquisition task terminated with error: {}", e),
            }
        }
        
        // Update driver status
        {
            let mut inner = self.inner.lock().map_err(|_|
                DriverError::Other("Failed to acquire lock on driver state".to_string()))?;
            inner.status = DriverStatus::Stopped;
        }
        
        // Notify about the status change
        self.notify_status_change().await?;
        info!("MockDriver acquisition stopped");
        Ok(())
    }

    /// Return the current driver status.
    ///
    /// This method returns the current status of the driver.
    /// If the lock cannot be acquired, it returns NotInitialized status.
    pub(crate) fn get_status(&self) -> DriverStatus {
        match self.inner.lock() {
            Ok(inner) => inner.status,
            Err(e) => {
                error!("Failed to acquire lock when getting status: {:?}", e);
                DriverStatus::NotInitialized
            }
        }
    }

    /// Shut down the driver.
    ///
    /// This method stops any ongoing acquisition and resets the driver state.
    ///
    /// # Important
    /// This method should always be called before the driver is dropped to ensure
    /// proper cleanup of resources. The Drop implementation provides only basic cleanup
    /// and cannot perform the full async shutdown sequence.
    pub(crate) async fn shutdown(&mut self) -> Result<(), DriverError> {
        debug!("Shutting down MockDriver");
        
        // First check if running, but don't hold the lock
        let should_stop = {
            match self.inner.lock() {
                Ok(inner) => inner.running,
                Err(e) => {
                    error!("Failed to acquire lock during shutdown: {:?}", e);
                    // Assume we need to stop acquisition to be safe
                    true
                }
            }
        };
        
        // Stop acquisition if needed
        if should_stop {
            debug!("Stopping acquisition as part of shutdown");
            self.stop_acquisition().await?;
        }

        // Update final state
        {
            let mut inner = self.inner.lock().map_err(|_|
                DriverError::Other("Failed to acquire lock on driver state".to_string()))?;
            inner.status = DriverStatus::NotInitialized;
            inner.config = None;
        }
        
        // Notify about the status change
        self.notify_status_change().await?;
        info!("MockDriver shutdown complete");
        Ok(())
    }

    /// Internal helper to notify status changes over the event channel.
    ///
    /// This method sends a status change event to any listeners.
    async fn notify_status_change(&self) -> Result<(), DriverError> {
        // Get current status with proper error handling
        let status = {
            match self.inner.lock() {
                Ok(inner) => inner.status,
                Err(e) => {
                    return Err(DriverError::Other(
                        format!("Failed to acquire lock for status notification: {:?}", e)
                    ));
                }
            }
        };
        
        debug!("Sending status change notification: {:?}", status);
        
        // Send the status change event
        self.tx
            .send(DriverEvent::StatusChange(status))
            .await
            .map_err(|e| DriverError::Other(format!("Failed to send status change: {}", e)))
    }
}

/// Helper function to generate dummy ADC data with sine waves for each channel.
/// Each channel's sine wave frequency is defined by:
///     channel 0: 2 Hz, channel 1: 6 Hz, channel 2: 10 Hz, etc.
/// (i.e., channel i gets 2 + 4*i Hz).
fn test_data(config: &AdcConfig, relative_micros: u64) -> AdcData {
    let t_secs = relative_micros as f32 / 1_000_000.0;
    trace!("Generating sample at t={} secs", t_secs);

    // For each channel, generate a sine wave sample based on its unique frequency.
    let samples: Vec<Vec<f32>> = config.channels.iter().enumerate().map(|(i, _)| {
        let freq = 2.0 + (i as f32) * 4.0; // 2 Hz for ch0, 6 Hz for ch1, etc.
        let angle = 2.0 * std::f32::consts::PI * freq * t_secs;
        let waveform = angle.sin();
        trace!("Channel {}: freq={} Hz, angle={} rad, value={}", i, freq, angle, waveform);
        vec![waveform]
    }).collect();

    // Use the absolute timestamp for the ADC data
    let timestamp = match current_timestamp_micros() {
        Ok(time) => time,
        Err(e) => {
            error!("Failed to get timestamp for ADC data: {:?}", e);
            // Fallback to a simpler method if the helper function fails
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_else(|_| std::time::Duration::from_secs(0))
                .as_micros() as u64
        }
    };
    
    AdcData { samples, timestamp }
}

// Implement the AdcDriver trait
#[async_trait]
impl super::types::AdcDriver for MockDriver {
    async fn shutdown(&mut self) -> Result<(), DriverError> {
        self.shutdown().await
    }

    async fn start_acquisition(&mut self) -> Result<(), DriverError> {
        self.start_acquisition().await
    }

    async fn stop_acquisition(&mut self) -> Result<(), DriverError> {
        self.stop_acquisition().await
    }

    fn get_status(&self) -> DriverStatus {
        self.get_status()
    }

    fn get_config(&self) -> Result<AdcConfig, DriverError> {
        self.get_config()
    }
}

/// Implementation of Drop for MockDriver to handle cleanup when the driver is dropped.
///
/// Note: This provides only basic cleanup. For proper cleanup, users should explicitly
/// call `shutdown()` before letting the driver go out of scope. The Drop implementation
/// cannot perform the full async shutdown sequence because Drop is not async.
impl Drop for MockDriver {
    fn drop(&mut self) {
        // Get the current status with proper error handling
        let status = match self.inner.lock() {
            Ok(inner) => inner.status,
            Err(e) => {
                error!("Failed to acquire lock in Drop implementation: {:?}", e);
                // Assume we need to clean up
                DriverStatus::Running
            }
        };
        
        // If the driver wasn't properly shut down, attempt to clean up
        if status != DriverStatus::NotInitialized {
            error!("MockDriver dropped without calling shutdown() first. This may lead to resource leaks.");
            error!("Always call driver.shutdown().await before dropping the driver.");
            
            // Try to stop any running acquisition
            if let Ok(mut inner) = self.inner.lock() {
                inner.running = false;
                inner.status = DriverStatus::NotInitialized;
                inner.config = None;
                debug!("Basic cleanup performed in Drop implementation");
            } else {
                error!("Failed to acquire lock for cleanup in Drop implementation");
            }
            
            // Note: We cannot await the task_handle here because Drop is not async.
            // This is why users should call shutdown() explicitly.
            if self.task_handle.is_some() {
                error!("Background task may still be running. Call shutdown() to properly terminate it.");
            }
        }
    }
}

// The following is a reference implementation for a real hardware driver.
// This is kept as documentation to show how a real hardware implementation
// might differ from the mock implementation.
/*
/// Driver for the ADS1299 EEG analog front-end chip.
/// This is a conceptual implementation that would be used with real hardware.
pub struct Ads1299Driver {
    spi: Spi,                     // SPI interface to communicate with the chip
    drdy_pin: Pin,                // Data Ready pin for interrupt-driven acquisition
    ring_buffer: RingBuffer,      // Circular buffer for samples
    sample_counter: u64,          // Track samples for timing
    tx: mpsc::Sender<DriverEvent>, // Event channel
    config: Option<AdcConfig>,    // Current configuration
    status: DriverStatus,         // Current status
}

impl Ads1299Driver {
    /// Create a new instance of the ADS1299 driver
    pub fn new() -> Result<(Self, mpsc::Receiver<DriverEvent>), DriverError> {
        // Initialize SPI, GPIO, etc.
        // ...
        
        let (tx, rx) = mpsc::channel(100);
        Ok((Self { /* ... */ }, rx))
    }
    
    /// Start data acquisition from the ADS1299
    async fn start_acquisition(&mut self) -> Result<(), DriverError> {
        // Set up DRDY interrupt handler for real-time data collection
        self.drdy_pin.set_interrupt_handler(move || {
            // Read data immediately when DRDY triggers
            let samples = self.spi.read_frame()?;
            self.ring_buffer.push(samples);
        });

        // Start continuous conversion mode by writing to the chip's registers
        self.write_register(REG_CONFIG1, START_CONTINUOUS_CONVERSION)?;
        
        // Spawn high-priority task for buffer management and event dispatch
        tokio::spawn(async move {
            while self.is_running() {
                // Read a batch of samples from the ring buffer
                let batch = self.ring_buffer.read_batch(32)?;
                
                // Send the batch to listeners
                if let Err(e) = self.tx.send(DriverEvent::Data(batch)).await {
                    error!("Failed to send batch: {}", e);
                    break;
                }
            }
        });
        
        Ok(())
    }
    
    // Other methods would be implemented similarly to MockDriver
    // but with real hardware interactions
}
*/