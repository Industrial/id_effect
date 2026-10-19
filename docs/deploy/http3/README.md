# HTTP/3 deployment examples

Examples for an `id_effect` service that serves HTTP/3 itself (topology (b) of book chapter
29.7, `crates/id_effect/book/src/part6/ch29-07-http3-deployment.md`). Behind a load balancer
that terminates HTTP/3 you need none of this: set `HTTP3=false` on the app, and add
`AltSvcLayer::h3(<balancer port>)` to the router only if the balancer does not send `Alt-Svc`
itself.

| File | What it is |
|---|---|
| `kamal.yml` | Kamal 2 `config/deploy.yml`: the web role without kamal-proxy, publishing `443/tcp` and `443/udp`, the ACME cache on a host directory |
| `helm-values.yaml` | Values for an app chart in the `helm create` layout: a LoadBalancer Service and container ports with `443/tcp` next to `443/udp`, a NetworkPolicy for both, probes over HTTPS, the ACME cache on a PersistentVolumeClaim, the sysctls |

Both assume the app binds `PORT=443` with HTTP/3 on (the default once it has TLS) and gets its
certificate from in-app ACME (`AcmeConfig::new([...], "/var/lib/app/acme")`) or from
`TLS_CERT_FILE` / `TLS_KEY_FILE`.

## What to change

- Kamal: `service`, `image`, `servers.web.hosts`, `registry`. Create the ACME directory on each
  host before the first deploy: `install -d -m 0700 -o <app uid> /var/lib/app/acme`.
- Helm: `image`, `runAsUser`/`fsGroup` (the image's user), `service` annotations for your cloud
  balancer (it must be layer 4 and support TCP and UDP on one port), `persistence.size`.
- The domain set, contact and `.production()` live in the app's `AcmeConfig`, not here.

## Template hooks the Helm values need

A chart created with `helm create` passes most of these blocks through already. Add:

- `containerPorts` into the container's `ports`, and `service.ports` into the Service (two
  entries with the same `port`, one `TCP` and one `UDP`);
- `service.externalTrafficPolicy` into the Service spec;
- `podSecurityContext.sysctls` (part of the pod `securityContext`);
- `strategy` into the Deployment spec;
- a NetworkPolicy rendered from `networkPolicy` when `enabled`;
- a PersistentVolumeClaim and volume mount from `persistence`;
- a DaemonSet from `nodeTuning` when `enabled`: a privileged init container (`hostNetwork:
  true`, `securityContext.privileged: true`) running `sysctl -w` for each entry of
  `nodeTuning.sysctls`, then a pause container. Skip it if your node image or node pool already
  sets the values.

## The sysctls

`net.core.rmem_max` and `net.core.wmem_max` must be at least `4194304` on every node or host
that serves HTTP/3: the endpoint asks for 4 MiB UDP buffers and the kernel caps the request at
these values (the granted size, and any shortfall, is logged at `debug` on target
`id_effect_platform::quic`).

| Where | How |
|---|---|
| A VM or bare-metal host (Kamal) | `/etc/sysctl.d/90-quic.conf`, then `sysctl --system`. Docker refuses these as `--sysctl`. |
| Kubernetes, any cluster | Node level: the node image, the node pool's sysctl config (GKE `linuxConfig.sysctls`, AKS `linuxOSConfig.sysctls`, EKS launch template user data), or the `nodeTuning` DaemonSet. |
| Kubernetes, pod level | Only where the kubelet allows them as unsafe (`--allowed-unsafe-sysctls='net.core.*'`) and the kernel scopes them to the pod's network namespace; otherwise the pod is rejected (`SysctlForbidden`). Commented out in `helm-values.yaml`. |

`net.ipv4.ip_unprivileged_port_start=0` is a safe sysctl (allowed by default since Kubernetes
1.22); it lets the non-root app bind 443. Docker sets it in containers by default.

## Checklist

- [ ] `443/tcp` **and** `443/udp` open end to end: firewall or security group, publish or
      Service, NetworkPolicy.
- [ ] No port remapping: the app's `PORT` is the public port (`Alt-Svc` names the bound port).
- [ ] Nothing else holds the UDP port; otherwise the host fails to start with `bind udp ...`.
      `HTTP3=false` is the one switch to run without HTTP/3.
- [ ] Socket buffer sysctls raised to `4194304`.
- [ ] Health checks over TCP (`/ready`, `/health`).
- [ ] ACME: `443/tcp` reachable from anywhere, TLS passed through to the app (`acme-tls/1`),
      cache directory on persistent storage (`0700`), one per replica, one `Acme` per domain
      set, an alert on `AcmeStats::failed` rising while `renew_at` is in the past. No DNS-01, no
      wildcards. Never the `id-effect dev` certificate in production (ADR 0014 C13).
- [ ] Nodes that also run the cluster mesh: decide on `quic.mesh_port` (book chapter 29.7, "Nodes
      that also run the mesh").

## Validating the files

```bash
yq e . docs/deploy/http3/kamal.yml docs/deploy/http3/helm-values.yaml > /dev/null
```
