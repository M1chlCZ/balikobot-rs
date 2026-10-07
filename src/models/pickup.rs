//! The pickup models shared by the ORDERPICKUP method.

/// One physical collection booking for ORDERPICKUP.
#[derive(Debug, Clone, PartialEq)]
pub struct PickupRequest {
    /// The collection date in the canonical "YYYY-MM-DD" format.
    pub date: String,
    /// The total collection weight in kilograms. It must be positive and at
    /// most 100000. DPD and DPDCZ send it; PPL takes the weight from its
    /// carrier configuration.
    pub weight_kg: f64,
    /// The number of packages. It must be positive and at most 10000. DPD and
    /// DPDCZ send it; PPL takes it from its carrier configuration.
    pub package_count: i32,
    /// The optional collection note. It must contain at most 255 valid
    /// characters without line breaks. DPD and DPDCZ send it as "message";
    /// PPL sends it as "note".
    pub note: String,
}

/// The confirmed collection booking.
#[derive(Debug, Clone, PartialEq)]
pub struct PickupResult {
    /// The provider pickup reference. PPL returns it; DPD and DPDCZ leave it
    /// empty.
    pub provider_id: String,
    /// The provider confirmation. DPD and DPDCZ always confirm on success.
    pub confirmed: bool,
}
