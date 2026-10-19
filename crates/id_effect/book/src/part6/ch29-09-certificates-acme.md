# Certificates that rotate themselves: reload and ACME

A service that terminates TLS itself — HTTPS on TCP, HTTP/3 on QUIC, a mesh between nodes — has
to replace its certificate before it expires, and it should not drop connections or restart to
do so. This chapter shows how `id_effect_platform` swaps certificates under running listeners,
and how it gets them from an ACME directory such as Let's Encrypt with the TLS-ALPN-01
challenge, on the TLS port the service already serves.

## What you'll learn

- How one `Identity` feeds the QUIC endpoint and the TCP listener, and what a swap changes.
- How to rotate certificates from files with `CertReloader`.
- How to issue and renew certificates with `Acme`, and wire its challenge into a TCP listener.
- What happens when an order fails, and where the keys live.

## Prerequisites

The `tls` feature of `id_effect_platform` (and `quic` for the QUIC endpoint, `acme` for ACME).
The QUIC endpoint is introduced in the transport-security chapter of part 7.

## One certificate slot for every listener

An `Identity` is a slot holding the certificate and key a listener presents. It is also the
rustls certificate resolver, so a listener asks it for the certificate on **every handshake**.
Hand clones of the same identity to everything that serves TLS: a swap is then seen by the next
handshake on all of them.

```rust,no_run
use std::sync::Arc;
use id_effect_platform::quic::{QuicEndpoint, QuicEndpointConfig, Route};
use id_effect_platform::tls::{Identity, NodeCertificate, server_tls_config};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let identity = Identity::from_certificate(&NodeCertificate::from_pem_files(
  "/etc/app/tls/cert.pem",
  "/etc/app/tls/key.pem",
)?)?;

// HTTP/3 and the mesh: one UDP port.
let endpoint = QuicEndpoint::bind(
  "0.0.0.0:443".parse()?,
  QuicEndpointConfig::new(identity.clone()),
)?;
let _h3 = endpoint.register(Route::h3())?;

// HTTPS on TCP: the same slot as the endpoint.
let tcp = Arc::new(server_tls_config(&identity, None, &[])?);
# let _ = tcp;
# Ok(()) }
```

What a swap changes:

| Connection | Certificate after a swap |
|---|---|
| New, accepted or dialed | the new one |
| Already open (TCP or QUIC) | the one it was established with; it is not cut |

A TLS session never re-reads its certificate, so nothing has to be drained or restarted.

## Rotating from files

When something else renews the certificate — cert-manager, a Vault agent, a cron job running
certbot — let `CertReloader` watch the two PEM files. It compares their contents (not their
modification times) and installs a new pair after checking that the key belongs to the leaf. A
half-written file is reported and the current certificate stays until the next poll.

```rust,no_run
use std::sync::Arc;
use std::time::Duration;
use id_effect_platform::tls::CertReloader;

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let reloader = Arc::new(CertReloader::new("/etc/app/tls/cert.pem", "/etc/app/tls/key.pem")?);
let identity = reloader.identity(); // hand clones to the endpoint and the TCP listener
let _polling = reloader.spawn_polling(Duration::from_secs(30)); // stops when dropped
# let _ = identity;
# Ok(()) }
```

To install a certificate you obtained some other way, call `identity.install(&cert)`: it
returns the new fingerprint, or an error and no change.

## Certificates from ACME (TLS-ALPN-01)

With feature `acme`, `Acme` obtains the certificate itself. It proves to the directory that it
controls each domain with the **TLS-ALPN-01** challenge: the directory opens a TLS connection to
the domain on port 443 offering only the `acme-tls/1` protocol, and the server answers with a
special self-signed certificate. That happens on the HTTPS port you already serve, so there is
no port 80 to open (HTTP-01) and no DNS provider to integrate (DNS-01, which is therefore not
supported; nor are wildcard names, which only DNS-01 can prove).

```rust,no_run
use std::sync::Arc;
use id_effect_platform::tls::acme::{Acme, AcmeConfig, is_acme_challenge};
use id_effect_platform::tls::server_tls_config_with;

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let acme = Acme::new(
  AcmeConfig::new(["app.example.com"], "/var/lib/app/acme")
    .production() // omit in staging and dev: Let's Encrypt staging is the default
    .contact("mailto:ops@example.com")
    .agree_to_terms_of_service(),
)?;

// The TCP listener answers the challenge; everyone else gets the current certificate.
let mut tcp = server_tls_config_with(Arc::new(acme.resolver()), None)?;
tcp.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
acme.serve_tls_alpn01(&mut tcp);

// The QUIC endpoint presents the same certificate.
let identity = acme.identity();

// Issue now if nothing is cached, then renew at two thirds of each lifetime.
let _renewals = acme.spawn();
# let _ = (tcp, identity, is_acme_challenge(None));
# Ok(()) }
```

After each TCP handshake, check `is_acme_challenge(conn.alpn_protocol())` and close the
connection if it is true: the validator has seen the certificate, and it must not be served the
application. The QUIC endpoint needs nothing — validation happens over TCP. The TCP listener
must not *require* client certificates, or the validator cannot connect.

### Renewal and failure

`Acme::spawn` renews at two thirds of the certificate's lifetime, minus a random jitter of up to
a twentieth of it, so a fleet of replicas does not hit the directory at the same moment. For a
90-day certificate that is between day 55.5 and day 60. `Acme::stats()` reports the counts and
the times:

| Field | Meaning |
|---|---|
| `issued` | certificates issued and installed |
| `failed` | orders that failed |
| `not_after` | expiry of the installed certificate (`None` while the placeholder is shown) |
| `renew_at` | when the next renewal is due |

A failed order — the directory is unreachable, a challenge is refused, the issued certificate
does not name every domain — changes nothing: the current certificate stays installed, the
failure is logged as a warning and counted in `failed`, and the task tries again after
`AcmeConfig::retry_after_failure` (one hour by default). Alert on `failed` growing while
`renew_at` is in the past: with a 90-day certificate you have about 30 days to fix it.

Until the first certificate is issued, `Acme` presents a self-signed placeholder for the first
domain. On restart it presents the cached certificate if it is unexpired and names every
configured domain, and renews only when that one is due.

### Keys on disk

`AcmeConfig::new` takes a cache directory. `Acme` creates it with mode `0700` and keeps there:

| File | Content | Mode |
|---|---|---|
| `account-<directory>.json` | the ACME account credentials, including the account key, one per directory URL | `0600` |
| `key.pem` | the private key of the current certificate | `0600` |
| `cert.pem` | the certificate chain | `0600` |

A key file found with a looser mode is tightened to `0600` (with a warning). In memory the keys
are held as `id_effect_config::Secret` values, zeroized when dropped, and neither appears in
`Debug` output or in logs. Mount the cache directory on persistent storage: losing it means a new
account and a new order on every start, and Let's Encrypt rate-limits both.

### Testing against Pebble

Point `directory_url` at a [Pebble](https://github.com/letsencrypt/pebble) container and trust its
test CA with `directory_root`:

```rust,no_run
use id_effect_platform::tls::acme::AcmeConfig;

let config = AcmeConfig::new(["app.test"], "/tmp/acme")
  .directory_url("https://localhost:14000/dir")
  .directory_root("/tmp/pebble.minica.pem")
  .agree_to_terms_of_service();
# let _ = config;
```

Pebble validates against its own `tlsPort` setting; set it to the port of your TCP listener and
resolve the test domain to that host.

## Summary

- One `Identity` per process feeds the QUIC endpoint and the TCP listener; a swap reaches new
  connections only, and open ones are not cut.
- `CertReloader` rotates from files; `Identity::install` swaps by hand.
- `Acme` issues and renews over TLS-ALPN-01 on the TLS port, renews at two thirds of the lifetime
  with jitter, keeps the current certificate when an order fails, and stores its keys with mode
  `0600`.
