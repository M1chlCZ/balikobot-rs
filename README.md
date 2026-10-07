# balikobot

A Rust client for the Balíkobot shipping API v2. The client is blocking and
covers branches, packages, labels, tracking, pickup orders and account
capabilities. The default endpoint is `https://apiv2.balikobot.cz`.

## Install

```sh
cargo add balikobot
```

Only a loopback test server can use the `http` scheme. Every other base URL
must use `https`.

## Quick start

Create a client with your API user and API key. The client sends the
credentials as HTTP Basic authentication. Then add a package, get a label URL
and read the tracking status.

```rust
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
```

## Methods

| Method | Endpoint | Purpose |
| --- | --- | --- |
| `branches` | `GET /{carrier}/branches/...` | Lists the branches of a service and country |
| `add_package` | `POST /{carrier}/add` | Creates one package. ADD is idempotent on `eid` |
| `overview` | `GET /{carrier}/overview` | Lists the packages that ORDER has not closed |
| `labels` | `POST /{carrier}/labels` | Gets a fresh label URL for one package |
| `order_view_labels` | `GET /{carrier}/orderview/{order_id}` | Gets the label URL of a closed order |
| `download_label` | `GET` the label URL | Downloads the label body |
| `track_status` | `POST /{carrier}/trackstatus` | Reads the tracking status of one package |
| `order_batch` | `POST /{carrier}/order` | Hands one package to the carrier batch |
| `drop_package` | `POST /{carrier}/drop` | Removes one package before ORDER |
| `order_pickup` | `POST /{carrier}/orderpickup` | Books one physical collection |
| `who_am_i` | `GET /info/whoami` | Reads the account and carrier data |
| `activated_services` | `GET /{carrier}/activatedservices` | Lists the activated services |
| `countries` | `GET /{carrier}/countries4service` | Lists the destination countries |
| `cod` | `GET /{carrier}/cod4services` | Lists the cash-on-delivery destinations |
| `carrier_capabilities` | `GET` the discovery endpoints | Discovers the contracted carriers and services |
| `resolve_branch_id` | none | Chooses the branch id or the branch zip for an ADD request |

## Codes

Carrier, currency and country values are typed, not plain strings:

| Type | Format | Common constants |
| --- | --- | --- |
| `CarrierCode` | `^[a-z0-9]{2,32}$` | `PPL`, `DPD`, `DPDCZ`, `DPDSK`, `GEIS`, `GLS`, `INTIME`, `CP`, `CESKAPOSTA`, `BALIKOVNA`, `ZASILKOVNA`, `SP`, `ULOZENKA` |
| `CurrencyCode` | ISO 4217 `^[A-Z]{3}$` | `CZK`, `EUR`, `USD`, `GBP`, `PLN`, `HUF`, `RON`, `BGN`, `HRK`, `CHF`, `NOK`, `SEK`, `DKK` |
| `CountryCode` | ISO 3166-1 alpha-2 `^[A-Z]{2}$` | EU member states plus `GB`, `CH`, `NO`, `IS`, `LI`, `UA`, `RS`, `BA`, `ME`, `MK`, `AL`, `TR`, `US`, `CA` |

Every type has a `new` constructor, an `is_valid` check and an `as_str`
accessor. `new` trims whitespace and normalizes the case, so custom carriers,
currencies and countries work:

```rust
let custom = CarrierCode::new("MyCarrier99")?;
let result = client.add_package(&custom, &request)?;
```

A function that expects a `CarrierCode` rejects a bare string, and a mistyped
constant fails the build. The JSON form stays a plain string, so the wire
contract does not change.

ADD still accepts only `CurrencyCode::CZK` and `CurrencyCode::EUR` as
`cod_currency`, because the carriers require one of those two values. The
client rejects other well-formed currency codes before the request.

## Errors

The client returns six sentinel errors:

| Error | Meaning | Action |
| --- | --- | --- |
| `Error::InvalidRequest` | The arguments are not valid. The client sent no request. | Correct the input. Do not retry. |
| `Error::Rejected` | The provider refused the data permanently. | Correct the data. Do not retry. |
| `Error::Unavailable` | The provider is unavailable, or the request never left the client. | Retry later. |
| `Error::NotFound` | The carrier has no tracking data yet. | Poll again later. |
| `Error::Ambiguous` | A mutating call can have reached the provider. | Reconcile with `overview`. Then retry. |
| `Error::InvalidResponse` | The answer violates the protocol. | Inspect the provider. Do not retry blindly. |

`Error::Unavailable` carries an optional `retry_after` field. The field holds
the provider `Retry-After` hint. Read the hint with `Error::retry_after`:

```rust
if let balikobot::Error::Unavailable { retry_after: Some(wait) } = error {
    std::thread::sleep(wait);
}
```

`Client::new` returns `Error::Config` when the configuration is not valid.
Correct the configuration.

The client sends no automatic retry. The caller controls the retry policy.

## Response limits

The client reads every JSON body with a hard limit of 8 MiB. Set
`Config::with_max_response_bytes` to change the limit. Label downloads use a
fixed limit of 4 MiB. The client refuses redirects. It compares the response
`Content-Type` with the expected media type before it decodes the body.

## Account mode

Set `Config::with_live_account(true)` or `Config::with_live_account(false)` to
verify the account before each mutating call. The client calls `who_am_i` and
compares the `live_account` flag. A mismatch blocks the write before the
client sends it. A successful result stays valid for five minutes. Without
this option, the client skips the check.

## Label hosts

The client accepts label URLs only from the Balíkobot label hosts, or from
the base URL origin for a loopback test server. The default allowlist is
`pdf.balikobot.cz` and every subdomain of `balikobot.cz`. Set
`Config::with_label_hosts` to replace the default allowlist with other hosts.
A leading dot selects a subdomain suffix match; it does not match the bare
domain.

## Development

Run the checks from the crate root:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The tests use loopback TCP servers. They use no real credentials and no
external network.

## License

MIT. See [LICENSE](LICENSE).
