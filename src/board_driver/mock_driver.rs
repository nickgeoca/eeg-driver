use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use async_trait::async_trait;
use log::{info, warn};
use super::types::{AdcConfig, AdcData, DriverStatus, DriverError, DriverEvent};

/// A stubbed-out driver that does not access any hardware.
pub struct MockDriver {
    inner: Arc<Mutex<MockInner>>,
    task_handle: Option<JoinHandle<()>>,
    tx: mpsc::Sender<DriverEvent>,
}

/// Internal state for the MockDriver.
struct MockInner {
    config: Option<AdcConfig>,
    running: bool,
    status: DriverStatus,
}

impl MockDriver {
    /// Create a new instance of the MockDriver.
    pub fn new() -> Result<(Self, mpsc::Receiver<DriverEvent>), DriverError> {
        let inner = MockInner {
            config: None,
            running: false,
            status: DriverStatus::NotInitialized,
        };
        let (tx, rx) = mpsc::channel(100);
        let driver = MockDriver {
            inner: Arc::new(Mutex::new(inner)),
            task_handle: None,
            tx,
        };
        Ok((driver, rx))
    }

    /// Reset and start the mock ADC driver with the given configuration.
    pub(crate) async fn reset_and_start(&mut self, config: AdcConfig) -> Result<(), DriverError> {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.config = Some(config);
            inner.status = DriverStatus::Ok;
        }
        // TODO add code to make sure the config.mock does not change after initialization
        self.notify_status_change().await?;
        info!("MockDriver initialized with new configuration");
        Ok(())
    }

    /// Return the current configuration.
    pub(crate) fn get_config(&self) -> Result<AdcConfig, DriverError> {
        let inner = self.inner.lock().unwrap();
        inner.config.clone().ok_or(DriverError::NotConfigured)
    }

    /// Start a dummy acquisition task that sends fake data at regular intervals.
    pub(crate) async fn start_acquisition(&mut self) -> Result<(), DriverError> {
        {
            let inner = self.inner.lock().unwrap();
            if inner.running {
                return Err(DriverError::ConfigurationError("Acquisition already running".to_string()));
            }
            if inner.config.is_none() {
                return Err(DriverError::NotConfigured);
            }
        }
        {
            let mut inner = self.inner.lock().unwrap();
            inner.running = true;
            inner.status = DriverStatus::Running;
        }
        self.notify_status_change().await?;

        let inner_arc = self.inner.clone();
        let tx = self.tx.clone();
        // Spawn a task that periodically sends dummy data.
        let handle = tokio::spawn(async move {
            let config = inner_arc.lock().unwrap().config.clone().unwrap();
            let batch_size = 32;  // Typical SPI buffer size
            
            // Get initial time as our zero reference
            let start_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64;
            
            while inner_arc.lock().unwrap().running {
                let mut batch = Vec::with_capacity(batch_size);
                let sample_interval = (1_000_000 / config.sample_rate) as u64; // microseconds between samples
                println!("Sample interval: {} microseconds", sample_interval);
                
                // Get current timestamp relative to start time
                let base_timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_micros() as u64;
                let relative_base = base_timestamp.saturating_sub(start_time);
                
                // Generate a batch of samples with incrementing timestamps
                for i in 0..batch_size {
                    let relative_timestamp = relative_base + i as u64 * sample_interval;
                    println!("Sample {}: relative_time={} microseconds", i, relative_timestamp);
                    let sample = test_data(&config, relative_timestamp);
                    batch.push(sample);
                }
                
                if let Err(e) = tx.send(DriverEvent::Data(batch)).await {
                    warn!("MockDriver event channel closed: {}", e);
                    break;
                }
                
                // Sleep for the time it would take to collect this batch via SPI
                sleep(Duration::from_millis((1000 * batch_size as u64) / config.sample_rate as u64)).await;
            }
        });
        self.task_handle = Some(handle);
        info!("MockDriver acquisition started");
        Ok(())
    }

    /// Stop the dummy data acquisition.
    pub(crate) async fn stop_acquisition(&mut self) -> Result<(), DriverError> {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.running = false;
        }
        if let Some(handle) = self.task_handle.take() {
            let _ = handle.await;
        }
        {
            let mut inner = self.inner.lock().unwrap();
            inner.status = DriverStatus::Stopped;
        }
        self.notify_status_change().await?;
        info!("MockDriver acquisition stopped");
        Ok(())
    }

    /// Return the current driver status.
    pub(crate) fn get_status(&self) -> DriverStatus {
        let inner = self.inner.lock().unwrap();
        inner.status
    }

    /// Shut down the driver.
    pub(crate) async fn shutdown(&mut self) -> Result<(), DriverError> {
        // First check if running, but don't hold the lock
        let should_stop = {
            let inner = self.inner.lock().unwrap();
            inner.running
        };
        
        // Stop acquisition if needed
        if should_stop {
            self.stop_acquisition().await?;
        }

        // Update final state
        {
            let mut inner = self.inner.lock().unwrap();
            inner.status = DriverStatus::NotInitialized;
            inner.config = None;
        }
        self.notify_status_change().await?;
        info!("MockDriver shutdown");
        Ok(())
    }

    /// Internal helper to notify status changes over the event channel.
    async fn notify_status_change(&self) -> Result<(), DriverError> {
        let status = {
            let inner = self.inner.lock().unwrap();
            inner.status
        };
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
    println!("Generating sample at t={} secs", t_secs);

    // For each channel, generate a sine wave sample based on its unique frequency.
    let samples: Vec<Vec<f32>> = config.channels.iter().enumerate().map(|(i, _)| {
        let freq = 2.0 + (i as f32) * 4.0; // 2 Hz for ch0, 6 Hz for ch1, etc.
        let angle = 2.0 * std::f32::consts::PI * freq * t_secs;
        let waveform = angle.sin();
        println!("Channel {}: freq={} Hz, angle={} rad, value={}", i, freq, angle, waveform);
        vec![waveform]
    }).collect();

    // Use the absolute timestamp for the ADC data
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64;
    
    AdcData { samples, timestamp: now }
}

#[async_trait]
impl super::types::AdcDriver for MockDriver {
    async fn reset_and_start(&mut self, config: AdcConfig) -> Result<(), DriverError> {
        self.reset_and_start(config).await
    }

    #[inline]
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

impl Drop for MockDriver {
    fn drop(&mut self) {
        let status = {
            let inner = self.inner.lock().unwrap();
            inner.status
        };
        if status != DriverStatus::NotInitialized {
            eprintln!("Warning: MockDriver dropped without calling shutdown() first.");
            let mut inner = self.inner.lock().unwrap();
            inner.running = false;
        }
    }
}

/*
// Conceptual structure for real hardware
struct Ads1299Driver {
    spi: Spi,
    drdy_pin: Pin,
    ring_buffer: RingBuffer,  // Circular buffer for samples
    sample_counter: u64,      // Track samples for timing
}

impl Ads1299Driver {
    async fn start_acquisition(&mut self) -> Result<(), DriverError> {
        // Set up DRDY interrupt handler
        self.drdy_pin.set_interrupt_handler(move || {
            // Read data immediately when DRDY triggers
            let samples = self.spi.read_frame()?;
            self.ring_buffer.push(samples);
        });

        // Start continuous conversion mode
        self.write_register(REG_CONFIG1, START_CONTINUOUS_CONVERSION)?;
        
        // Spawn high-priority task for buffer management
        tokio::spawn(async move {
            while self.is_running() {
                let batch = self.ring_buffer.read_batch(32)?;
                if let Err(e) = self.tx.send(DriverEvent::Data(batch)).await {
                    error!("Failed to send batch: {}", e);
                }
            }
        });
    }
}
*/