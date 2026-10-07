//! The public data models returned by the API methods.

pub mod branch;
pub mod capabilities;
pub mod pickup;
pub mod shipment;
pub mod tracking;

pub use branch::Branch;
pub use capabilities::{
    ActivatedServices, CODCapability, Carrier, Service, ServiceCOD, ServiceCountries, WhoAmI,
    WhoAmICarrier,
};
pub use pickup::{PickupRequest, PickupResult};
pub use shipment::{AddPackageRequest, AddPackageResult, OverviewPackage};
pub use tracking::{OrderResult, TrackStatusResult};
