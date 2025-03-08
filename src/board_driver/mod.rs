pub mod mock_driver;
pub mod types;

// Re-export types for convenience
pub use self::types::{AdcData, AdcConfig, DriverEvent, DriverStatus, DriverError, AdcDriver, DriverType};
pub use self::mock_driver::MockDriver;
pub use self::types::create_driver;