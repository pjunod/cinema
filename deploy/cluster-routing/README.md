# Cluster ingress patterns

These examples put one stable address in front of healthy plurx voters while
preserving node-specific origins for native-client failover. They are starting
points: replace every example address, certificate, interface, and secret for
your network before enabling one.

## HAProxy

Copy `haproxy.cfg.example` into an existing HAProxy deployment. It uses
`/readyz`, ejects a voter only after repeated failures, and gives browsers a
sticky cookie for HLS/cache locality. HAProxy does not replay failed writes;
clients decide whether a request is safe to retry.

## keepalived

Install `keepalived.conf.example` on each bare-metal voter, changing `state`,
`priority`, `interface`, `unicast_src_ip`, and peers per host. The VIP moves
only to a machine whose local `/readyz` succeeds. This is failover, not load
distribution; use HAProxy behind the VIP when both are required.

## Kubernetes

`kubernetes-service.yaml` supplies a sticky Service and an optional
ingress-nginx Ingress. The plurxd workload remains responsible for its
readiness probe, anti-affinity, stable data, media mounts, GPU resources, and
the private Raft/API ports. Do not scale a shared data directory as though it
were stateless storage.

For rollout, drain one backend at a time and follow the authoritative procedure
in [OPERATIONS.md](../../docs/OPERATIONS.md#cluster-ingress-drain-and-recovery).
