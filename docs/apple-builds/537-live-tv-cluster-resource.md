# Live TV cluster resource

**Status:** built

Build: 192
Issue: #537

Network tuners are shared cluster resources. Apple clients negotiate protocol 5
and obtain durable start intents before playback, preserving recovery across
ingress changes. Protocol 4 belongs to the earlier owner-relay implementation
and is deliberately not advertised as equivalent.

The Developer enable control remains available with advisory readiness details.
PR #537 records the final review and fast-lane result. Device playback and fleet
deployment are not claimed by these compile and unit checks.
