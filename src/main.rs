mod board_driver;

use tokio;
use std::error::Error;
use clap::Parser;
use eeg_driver::{AdcConfig, EegSystem};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Run in mock mode with random data
    #[arg(long)]
    mock: bool,

    /// Sample rate in Hz
    #[arg(long, default_value_t = 250)]
    sample_rate: u32,

    /// Channels to read (comma-separated)
    #[arg(long, value_delimiter = ',', default_values_t = vec![0, 1, 2, 3])]
    channels: Vec<usize>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    // Create a basic ADC configuration
    let config = AdcConfig {
        sample_rate: 250,
        channels: vec![0, 1, 2, 3],
        gain: 24.0,
        mock: true,
    };

    // Create the EEG system (using mock driver)
    let (mut eeg_system, mut data_rx) = EegSystem::new(config.clone()).await?;
    
    // Start the system
    eeg_system.start(config).await?;

    // Example: Process received data for a while
    while let Some(processed_data) = data_rx.recv().await {
        println!("Received data with {} channels", processed_data.channel_count);
        println!("Data: {:?}", processed_data); 
        // Add your data handling logic here
    }

    // Stop the system when done
    eeg_system.stop().await?;

    Ok(())
}
