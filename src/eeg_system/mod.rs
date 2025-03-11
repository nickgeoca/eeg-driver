use std::error::Error;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex}; // Use Tokio Mutex
use tokio::task::JoinHandle;
use std::time::Duration;

use crate::board_driver::{
    create_driver, AdcConfig, AdcDriver, DriverError, DriverEvent, DriverStatus, DriverType,
};
use crate::dsp::filters::SignalProcessor;
use super::ProcessedData;

pub struct EegSystem {
    driver: Box<dyn AdcDriver>,
    processor: Arc<Mutex<SignalProcessor>>,
    processing_task: Option<JoinHandle<()>>,
    tx: mpsc::Sender<ProcessedData>,
    event_rx: Option<mpsc::Receiver<DriverEvent>>,
}

impl EegSystem {
    /// Creates an EEG processing system without starting it
    pub async fn new(
        config: AdcConfig
    ) -> Result<(Self, mpsc::Receiver<ProcessedData>), Box<dyn Error>> {
        let (driver, event_rx) = create_driver(config.clone()).await?;
        let processor = Arc::new(Mutex::new(SignalProcessor::new(
            config.sample_rate,
            config.channels.len(),
        )));
        let (tx, rx) = mpsc::channel(100);

        let system = Self {
            driver,
            processor,
            processing_task: None,
            tx,
            event_rx: Some(event_rx),
        };

        Ok((system, rx))
    }

    /// Starts the processing system with the given configuration
    pub async fn start(&mut self, config: AdcConfig) -> Result<(), Box<dyn Error>> {
        self.initialize_processing(config).await
    }

    /// Internal helper to initialize or reinitialize the driver and processing task
    async fn initialize_processing(&mut self, config: AdcConfig) -> Result<(), Box<dyn Error>> {
        // Add validation before proceeding
        if config.channels.is_empty() {
            return Err(Box::new(DriverError::ConfigurationError(
                "Cannot initialize with zero channels".into()
            )));
        }

        if config.sample_rate == 0 {
            return Err(Box::new(DriverError::ConfigurationError(
                "Sample rate must be greater than 0".into()
            )));
        }

        // Stop any existing processing task gracefully
        if let Some(task) = self.processing_task.take() {
            task.abort();
        }

        // Reset the signal processor
        {
            let mut proc_guard = self.processor.lock().await;
            proc_guard.reset(config.sample_rate, config.channels.len());
        }

        self.driver.start_acquisition().await?;

        // Take ownership of the event receiver
        let mut event_rx = self.event_rx.take().expect("Event receiver should exist");

        // Start the processing task
        let processor: Arc<Mutex<SignalProcessor>> = Arc::clone(&self.processor);
        let tx = self.tx.clone();

        self.processing_task = Some(tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    DriverEvent::Data(data_batch) => {
                        // Pre-allocate with known capacity
                        let batch_size = data_batch.len();
                        let channel_count = data_batch[0].samples.len();
                        let samples_per_channel = data_batch[0].samples[0].len();
                        
                        let mut processed_channels: Vec<Vec<f32>> = Vec::with_capacity(channel_count);
                        for _ in 0..channel_count {
                            processed_channels.push(Vec::with_capacity(batch_size * samples_per_channel));
                        }
                        
                        // Single lock acquisition for the batch
                        let mut proc_guard = processor.lock().await;
                        
                        // Process all samples in the batch
                        for data in &data_batch {
                            for (ch_idx, channel_samples) in data.samples.iter().enumerate() {
                                processed_channels[ch_idx].extend(
                                    channel_samples.iter().map(|&sample| 
                                        proc_guard.process_sample(ch_idx, sample as f32)
                                    )
                                );
                            }
                        }
                        drop(proc_guard);

                        if tx.send(ProcessedData {
                            data: processed_channels,
                            timestamp: data_batch.last().unwrap().timestamp,
                            channel_count,
                        }).await.is_err() {
                            break;
                        }
                    }
                    DriverEvent::StatusChange(status) => {
                        if status == DriverStatus::Stopped {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }));

        Ok(())
    }

    /// Stop the data acquisition & abort the background task
    pub async fn stop(&mut self) -> Result<(), Box<dyn Error>> {
        self.driver.stop_acquisition().await?;
        if let Some(task) = self.processing_task.take() {
            task.abort();
        }
        Ok(())
    }

    /// Reconfigure the driver with new settings, resetting the processor
    pub async fn reconfigure(&mut self, config: AdcConfig) -> Result<(), Box<dyn Error>> {
        self.initialize_processing(config).await
    }

    /// Retrieve the current driver status
    pub async fn driver_status(&self) -> DriverStatus {
        self.driver.get_status().await
    }

    /// Retrieve the driver's configuration
    pub async fn driver_config(&self) -> Result<AdcConfig, DriverError> {
        self.driver.get_config().await
    }

    /// Optionally allow direct driver access
    pub fn driver(&mut self) -> &mut Box<dyn AdcDriver> {
        &mut self.driver
    }

    /// Completely shut down the EEG system and clean up resources
    pub async fn shutdown(&mut self) -> Result<(), Box<dyn Error>> {
        // Add timeout for safety
        const SHUTDOWN_TIMEOUT_MS: u64 = 1000;
        
        let shutdown_future = async {
            // Convert the Box<dyn Error> to DriverError
            if let Err(e) = self.stop().await {
                return Err(DriverError::Other(e.to_string()));
            }
            self.driver.shutdown().await
        };
        
        match tokio::time::timeout(
            Duration::from_millis(SHUTDOWN_TIMEOUT_MS),
            shutdown_future
        ).await {
            Ok(result) => result.map_err(|e| Box::new(e) as Box<dyn Error>),
            Err(_) => Err(Box::new(DriverError::Other("Shutdown timed out".into())))
        }
    }
}

impl Drop for EegSystem {
    fn drop(&mut self) {
        // Since we can't use .await in Drop, we'll just log a warning
        eprintln!("Warning: EegSystem dropped without calling shutdown() first");
        eprintln!("Always call system.shutdown().await before dropping the system");
    }
}
