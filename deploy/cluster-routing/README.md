# Cluster ingress patterns

These examples put one stable address in front of healthy plurx nodes while
preserving node-specific origins for native-client failover. HAProxy,
keepalived, and Kubernetes syntax below is illustrative. The application
contract is vendor-neutral and applies equally to Caddy, nginx, Traefik, a
cloud load balancer, or a local appliance.

## Application contract

| Boundary | Required behavior |
|---|---|
| New traffic | Probe `GET /readyz`; start with a 10 s interval, 2 s timeout, and three failures before ejection. `/healthz` is process liveness only. |
| HLS and segments | Keep `/hls/` playlists and segments on one ready backend with a cookie or source hash. If it leaves rotation, move the next request to a survivor and preserve the playlist discontinuity. |
| Other requests | Distribution may be round-robin or least-connections. Do not route everything to the Raft leader. |
| Connection bound | Cap backend connection establishment at 2 s so an unavailable host cannot consume the recovery budget. |
| Drain bound | Stop new traffic immediately, preserve existing connections for at most 75 s, then close them before maintenance. Confirm node-local transcode/remux sessions separately. |
| Retry safety | A proxy may retry `GET` or `HEAD` after a connection failure. It must never automatically replay `POST`, `PUT`, `PATCH`, or `DELETE`; the client or application owns idempotency. |

Stickiness improves locality; it is not an availability dependency. A learner
is eligible only while `/readyz` and its declared route matrix permit the
request. It is never voter redundancy and must not receive authority mutations.

Run the executable local contract fixture without installing a proxy product:

```bash
cargo run --locked -p plurx-cluster-check -- proxy-fixture
```

`make cluster-check` also runs the fixture after real separate-process learner,
follower-loss, and leader-loss drills. CI retains the closed-schema result as
`target/validation/cluster-failure-drills.json`; only that artifact may support
an absolute recovery claim. Replace every example address, certificate,
interface, and secret below for your network before enabling one.

## HAProxy

Copy `haproxy.cfg.example` into an existing HAProxy deployment. It illustrates
`/readyz`, ejects a voter only after repeated failures, and gives browsers a
sticky cookie for HLS/cache locality. Separate method-selected backends permit
bounded `GET`/`HEAD` redispatch while setting mutation retries to zero; HAProxy
does not replay failed writes, and clients decide whether a mutation is safe to
retry.

## keepalived

Install `keepalived.conf.example` on each bare-metal voter, changing `state`,
`priority`, `interface`, `unicast_src_ip`, and peers per host. The VIP moves
only to a machine whose local `/readyz` succeeds. This is failover, not load
distribution; use HAProxy behind the VIP when both are required.

## Kubernetes

`kubernetes-service.yaml` illustrates a sticky Service and an optional
ingress-nginx Ingress. The plurxd workload remains responsible for its
readiness probe, anti-affinity, stable data, media mounts, GPU resources, and
the private Raft/API ports. Do not scale a shared data directory as though it
were stateless storage.

For rollout, drain one backend at a time and follow the authoritative procedure
in [OPERATIONS.md](../../docs/OPERATIONS.md#cluster-ingress-drain-and-recovery).
