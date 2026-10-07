use balikobot::{AddPackageRequest, CarrierCode, Client, Config, CountryCode, CurrencyCode};

fn main() -> balikobot::Result<()> {
    let config = Config::new("api-user", "api-key");
    let client = Client::new(config)?;

    let result = client.add_package(
        &CarrierCode::PPL,
        &AddPackageRequest {
            eid: "order-2026-000123-S1".to_owned(),
            service_type: "1".to_owned(),
            rec_name: "Example Recipient".to_owned(),
            rec_firm: String::new(),
            rec_street: "Example 1".to_owned(),
            rec_city: "Praha".to_owned(),
            rec_zip: "11000".to_owned(),
            rec_country: CountryCode::CZ,
            rec_phone: "+420777000000".to_owned(),
            rec_email: String::new(),
            branch_id: String::new(),
            weight: 1.5,
            length: 30.0,
            width: 20.0,
            height: 10.0,
            price: 1000.0,
            cod_price: 0.0,
            cod_currency: CurrencyCode::CZK,
            vs: None,
        },
    )?;

    let label_url = client.labels(&CarrierCode::PPL, &result.package_id)?;
    println!("{label_url}");

    let status = client.track_status(&CarrierCode::PPL, &result.carrier_id)?;
    println!("{}", status.status_text);

    Ok(())
}
