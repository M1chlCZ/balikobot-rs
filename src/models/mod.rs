//! The public data models returned by the API methods.

pub mod branch;
pub mod shipment;

pub use branch::Branch;
pub use shipment::{AddPackageRequest, AddPackageResult, OverviewPackage};
