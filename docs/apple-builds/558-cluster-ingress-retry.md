# Retry media through another cluster ingress

Issue: #558

A stalled stream may retry the unchanged media path through another cluster
node's ingress. Only an explicit transport failure moves, the account origin
and codec-compatibility ladder stay put, and HTTPS never falls back to an HTTP
sibling.
