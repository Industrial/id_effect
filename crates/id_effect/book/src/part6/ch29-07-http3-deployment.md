# Deploying HTTP/3: load balancers, direct-to-app, Kamal and Helm

[Serving HTTP/3 from the app](./ch29-05-http3.md) shows what the host does once it has a
certificate. This chapter covers what has to be true around it: which ports are open, what a
load balancer must and must not do, which kernel settings matter, and how ACME certificates fit
into a fleet. The examples in `docs/deploy/http3/` (Kamal and Helm) put it into practice.

## What you'll learn

- The two topologies: a load balancer that terminates HTTP/3, and HTTP/3 straight to the app.
- Ports, firewall rules and health checks, and what happens when UDP is blocked.
- The UDP socket buffers, and why HTTP/3 runs BBR while the mesh stays on CUBIC.
- ACME in production: TLS passthrough, persistent keys, replicas, alerts.
- When a node that also runs the cluster mesh should split it onto its own port.

## Prerequisites

- [Serving HTTP/3 from the app](./ch29-05-http3.md): `HostConfig::http3`, `Alt-Svc`.
- [Certificates that rotate themselves](./ch29-09-certificates-acme.md): `Identity`, `Acme`.

## Two topologies

| | (a) Balancer terminates HTTP/3 | (b) Direct to the app |
|---|---|---|
| Who speaks QUIC to the browser | the balancer | the app |
| App setting | `HTTP3=false` | `HTTP3` left at its default (`true`) |
| Who sends `Alt-Svc` | the balancer, or `AltSvcLayer` in the app | the host, automatically |
| Certificates | at the balancer | in the app: files or ACME |
| Open on the app's port | TCP (from the balancer) | TCP **and** UDP, same port number |

A layer-4 balancer that forwards TCP and UDP without terminating TLS (a "network" or
"passthrough" balancer) counts as topology (b): the app still terminates TLS and QUIC itself.

### (a) A balancer that terminates HTTP/3

The balancer does HTTP/3 at the edge and talks HTTP/1.1 or HTTP/2 to the app. The app needs
nothing from this chapter. Turn its own HTTP/3 off:

```bash
HTTP3=false ./app        # or HostConfig { http3: false, .. }
```

With HTTP/3 off the host binds no UDP port and adds no `Alt-Svc` header. Leaving it on is
harmless when the internal hop is plain HTTP (without TLS the host serves no HTTP/3 anyway, but
logs the posture warning `HTTP3_WITHOUT_TLS_WARNING` at startup). When the internal hop is TLS
it would bind UDP on the app's port for nothing, and its `Alt-Svc` would name the app's port,
not the balancer's.

Most balancers that terminate HTTP/3 add `Alt-Svc` themselves. If yours does not, add it in the
app, with the port the **balancer** listens on:

```rust,no_run
use axum::Router;
use id_effect_axum::server::AltSvcLayer;

let app: Router = Router::new().layer(AltSvcLayer::h3(443)); // h3=":443"; ma=86400
# let _ = app;
```

Do not add `AltSvcLayer` when the balancer already sends `Alt-Svc`: browsers would see two
headers for one origin. Behind the balancer, configure it as a trusted proxy so `ClientIp` reads
the client address from `X-Forwarded-For` ([the client IP](./ch29-05-http3.md#the-client-ip)).

### (b) HTTP/3 straight to the app

The app terminates TLS on TCP and QUIC on UDP, on **the same port number**. With
`TLS_CERT_FILE`, `TLS_KEY_FILE` and `PORT=443` the host binds `443/tcp` first, then `443/udp`,
and serves the same router on both.

**Open both protocols on that port, end to end.** Firewall, security group, container port
publishing and Kubernetes Service all need `443/tcp` and `443/udp`:

```bash
# nftables
nft add rule inet filter input tcp dport 443 accept
nft add rule inet filter input udp dport 443 accept
```

**Do not remap the port.** The host advertises `Alt-Svc: h3=":<PORT>"`, the port it bound.
If a container listens on 8443 and is published on 443, browsers are told to try UDP 8443 on
the public address and never reach HTTP/3. Bind 443 inside the container (Docker allows
unprivileged ports by default; on Kubernetes set the safe sysctl
`net.ipv4.ip_unprivileged_port_start=0`, as the Helm example does) and publish `443:443` for
both protocols.

**A taken UDP port fails startup.** If something else holds the UDP port (another instance of
the app, a previous container still stopping, a DNS or VPN daemon), the host does not start at
all, with an error such as:

```text
bind udp 0.0.0.0:443: http3: ... Address already in use (os error 98)
```

It does not fall back to serving TCP only, so a misconfigured port is seen at deploy time, not
weeks later as "HTTP/3 never works". If the app should not serve HTTP/3 on that host,
`HTTP3=false` is the one switch: it turns off the UDP bind and the `Alt-Svc` header together.

**Health checks go over TCP.** The health routes (`/health`, `/ready`) answer on both
protocols, but balancer health checks are TCP or HTTP over TCP. One process serves both listeners from one router, so
the TCP check stands for the UDP side too. Keep readiness on `/ready`, which answers `503` while
the host drains.

**When UDP is blocked**, nothing breaks: browsers that cannot reach the UDP port keep using
HTTP/2 on TCP, and the platform `Http3Client` falls back after 300 ms and remembers the failure
for 5 minutes ([The HTTP/3 client](./ch29-08-http3-client.md#when-udp-is-blocked)). The cost of a
blocked UDP port is a lost upgrade, not an outage. The same holds the other way: after
`HTTP3=false`, browsers that cached `Alt-Svc` (`ma=86400`, a day) try UDP once, fail, and go back
to TCP.

## UDP socket buffers

A QUIC server receives every packet of every connection on one UDP socket. With the Linux
default buffer (often 208 KiB), a burst from many clients overflows it and the kernel drops
packets, which QUIC then retransmits. The endpoint asks for **4 MiB** each way
(`DEFAULT_SOCKET_BUFFER`); the kernel grants at most `net.core.rmem_max` (receive) and
`net.core.wmem_max` (send). Raise both to at least 4194304 on every host that serves HTTP/3:

```bash
# /etc/sysctl.d/90-quic.conf
net.core.rmem_max = 4194304
net.core.wmem_max = 4194304
```

```bash
sysctl --system
```

The request is best effort: the host starts either way. The granted size is logged at `debug`
on target `id_effect_platform::quic`, and a shortfall names the sysctl to raise:

```text
UDP receive buffer: 425984 bytes granted of 4194304 asked for; raise net.core.rmem_max (Linux) so bursts are not dropped
```

These two sysctls belong to the host kernel, not to a container. Docker does not accept them as
`--sysctl`; Kubernetes accepts them in a pod's `securityContext.sysctls` only when the kubelet
allows them as unsafe (`--allowed-unsafe-sysctls='net.core.*'`) and the kernel scopes them to
the pod's network namespace. On most clusters the reliable way is the node: a `sysctl.d` file in
the node image, the cloud's node configuration, or a privileged DaemonSet. Both examples show
where.

## Congestion control: BBR for HTTP/3, CUBIC for the mesh

The `h3` route runs **BBR**; the mesh (`Route::mesh`) and any other route (`Route::new`) run
**CUBIC**. At 1 % random loss the release spike measured a p99 of 31 ms for HTTP/3 with BBR,
1,054 ms with CUBIC and 609 ms for HTTP/2 ([the
numbers](./ch29-05-http3.md#performance-under-packet-loss)): a loss-based controller backs off on
every lost packet, BBR does not read random loss as congestion. Between mesh nodes in one data
centre loss is rare, and the mesh keeps the controller it had in 0.11.

quinn marks its BBR **experimental**. The choice is per route, and `Route::with_congestion_control`
changes it:

```rust
use id_effect_platform::quic::{CongestionControl, Route};

assert_eq!(Route::h3().congestion_control(), CongestionControl::Bbr);
let cubic_h3 = Route::h3().with_congestion_control(CongestionControl::Cubic);
assert_eq!(cubic_h3.congestion_control(), CongestionControl::Cubic);
```

The host and `serve_h3` register `Route::h3()` themselves and have no switch for it; a route with
another controller is for an endpoint whose `h3` route you register and serve yourself. On a
shared endpoint a connection starts on CUBIC and switches to its route's controller once the
handshake has chosen the ALPN.

## ACME in production

[`Acme`](./ch29-09-certificates-acme.md#certificates-from-acme-tls-alpn-01) issues and renews the
certificate from inside the app. The host takes it as provided TLS settings: the resolver answers
the challenge on TCP, and the identity is what HTTP/3 presents.

```rust,no_run
use std::sync::Arc;
use axum::Router;
use id_effect_axum::server::{HostBuilder, HostTls, ServerTls, serve_router};
use id_effect_platform::tls::acme::{Acme, AcmeConfig};

# async fn run(app: Router) -> Result<(), Box<dyn std::error::Error>> {
let acme = Acme::new(
  AcmeConfig::new(["app.example.com"], "/var/lib/app/acme")
    .production()
    .contact("mailto:ops@example.com")
    .agree_to_terms_of_service(),
)?;
let challenge = acme.clone();
let tls = ServerTls::builder(Arc::new(acme.resolver()))
  .configure(move |config| challenge.serve_tls_alpn01(config))
  .identity(acme.identity()) // without it the host serves TCP only
  .build()?;

let mut host = HostBuilder::new().build().await?;
host.config = host.config.clone().with_tls(HostTls::provided(tls));
let _renewals = acme.spawn(); // issue now if nothing is cached, then renew
serve_router(host, app).await?;
# Ok(()) }
```

The listener closes every connection that negotiated `acme-tls/1` right after its handshake, so
the validator never reaches the router.

### What the network must allow

- **TCP 443 reachable from the internet.** The directory validates by opening TLS to each domain
  on port 443, from several places it does not publish. Allow `443/tcp` from anywhere, not only
  from your users' networks.
- **`acme-tls/1` must reach the app.** The validator offers only that ALPN, and only the app can
  answer it. A balancer in front must pass TLS through (layer 4, TCP); one that terminates TLS
  answers the handshake itself and validation fails. With a terminating balancer, let it manage
  the certificate (topology a).
- **UDP 443 is for HTTP/3 only.** Validation runs over TCP; the QUIC endpoint needs nothing for
  ACME and only presents the issued certificate.

### Keys and state

- Put the cache directory (`AcmeConfig::new(_, "/var/lib/app/acme")`) on **persistent storage**.
  `Acme` creates it with mode `0700` and writes the account, `key.pem` and `cert.pem` with mode
  `0600`. On an ephemeral disk every restart creates a new account and a new order, and Let's
  Encrypt rate-limits both. Mount it writable for the app's user only.
- The default directory is Let's Encrypt **staging**; `.production()` switches to production.
  `directory_url` plus `directory_root` (a PEM file holding the only CA trusted for the
  connection to the directory) are for Pebble or a staging directory on a private PKI, not for
  production.

### Replicas

Run **one `Acme` per domain set** in a process. Replicas do not share a certificate: each replica
orders and renews its own, with its own cache directory (never one directory mounted into several
replicas). Two things follow:

- Behind a layer-4 balancer the validator's connection may land on a replica other than the one
  that placed the order. That replica has no pending challenge for it, so the order fails, is
  counted, and is retried after `retry_after_failure` (one hour by default).
- Let's Encrypt limits how many certificates it issues for the same set of names per week.

So in-app ACME fits a single instance or a few replicas. For more, give the certificate to the
balancer, or let cert-manager or a Vault agent write the files and serve them with
`TLS_CERT_FILE` / `TLS_KEY_FILE` (reloaded every `TLS_RELOAD_SECS`).

### What is not supported

There is no DNS-01 challenge and no wildcard certificate: TLS-ALPN-01 proves one DNS name at a
time and a wildcard can only be proven over DNS. `AcmeConfig` refuses `*.example.com` and IP
addresses. For a wildcard, obtain the certificate elsewhere and serve the files.

### Alerting

Export `Acme::stats()` to your metrics. A failed order keeps the current certificate and only
logs a warning, so the signal to watch is **`AcmeStats::failed` rising while `renew_at` is in
the past**: renewal is overdue and not succeeding. A 90-day certificate renews around day 55 to
60, which leaves about 30 days to fix it. `not_after` is `None` while the self-signed placeholder
is presented (before the first issuance).

### Never the development certificate

`id-effect dev` issues a short-lived development certificate. It must never reach production,
and the prod profile refuses it (ADR 0014, rule C13). Production gets its certificate from ACME,
from files your PKI writes, or from the balancer.

## Provided TLS settings and HTTP/3

`HostTls::Provided` serves HTTP/3 only when the settings carry an identity, because QUIC takes
an `Identity`, not a rustls resolver:

| Settings | TCP | HTTP/3 |
|---|---|---|
| `ServerTls::from_identity`, `ServerTls::from_pem_files` | yes | yes |
| `ServerTls::builder(resolver).identity(id)` (as above for ACME) | yes | yes |
| `ServerTls::builder(resolver)` without `.identity`, `ServerTls::from_rustls_config` | yes | no |

Without an identity the host serves TCP only, sends no `Alt-Svc`, and logs a warning on target
`id_effect_axum::posture` saying so. If you see that warning in topology (b), add `.identity`.

## Nodes that also run the mesh (D7)

A node that serves HTTP/3 and the cluster mesh (ALPN `idm/1`) can share one UDP port: one
`QuicEndpoint` routes by ALPN ([shared endpoint, ADR 0014
D7](../part7/ch38-01-transport-security.md)). rustls fixes the client-certificate rule per
endpoint, so on the shared port the handshake only *requests* a client certificate (no CA hints),
and the `idm/1` route refuses any connection without a verified client chain before reading a
frame. Browsers stay unaffected, except that one with installed client certificates may show a
certificate picker.

Set `quic.mesh_port` to give the mesh its own endpoint, with mutual TLS required in the handshake
as in 0.11:

```toml
[quic]
mesh_port = 4370
```

Split when:

- browsers reach the node directly and a certificate picker is unacceptable;
- the mesh must stay on a private network while 443 faces the internet: firewall the mesh port
  to the cluster's addresses only;
- you want mesh and public traffic counted, rate-limited or captured separately.

The `service` template has no mesh, so it never shares its HTTP/3 port with one. A
`cluster`/`worker` deployment that also serves HTTP/3 to browsers should set `quic.mesh_port`.

## The examples

`docs/deploy/http3/` holds:

- `kamal.yml`: a Kamal deployment that publishes `443/tcp` and `443/udp` straight to the app
  (Kamal's proxy does not forward UDP), with ACME state on a host volume and the sysctls set on
  the host;
- `helm-values.yaml`: values for an app chart, with a Service exposing `443/tcp` and `443/udp`, a
  NetworkPolicy allowing both, a persistent volume for the ACME cache and the sysctl options;
- `README.md`: which values to change, and a checklist.

## Summary

- Behind a balancer that terminates HTTP/3: `HTTP3=false`, and `AltSvcLayer` only if the
  balancer sends no `Alt-Svc` itself.
- Direct to the app: TCP and UDP on the same port, no remapping; a taken UDP port fails startup
  with `bind udp`; health checks over TCP; blocked UDP means HTTP/2, not an outage.
- Raise `net.core.rmem_max` and `net.core.wmem_max` to 4194304 on the host.
- HTTP/3 runs BBR (experimental in quinn), the mesh CUBIC; `Route::with_congestion_control`
  picks per route.
- ACME: TCP 443 and `acme-tls/1` must reach the app, persistent `0700` cache, one `Acme` per
  domain set, alert on `failed` with `renew_at` past, no DNS-01 or wildcards.
- `HostTls::Provided` needs an identity for HTTP/3; `quic.mesh_port` splits the mesh off.
