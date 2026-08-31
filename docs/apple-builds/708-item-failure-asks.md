# Ask before walking the compatibility ladder

Build: 100
Issue: #708

An item failure walked three rungs — another node, then established HDR, then
a compatibility transcode — guessing at retry the whole way. For a source the
producer has already ruled out, that is three failures to learn something the
server knew before the first one.

The ask goes before rung one rather than rung two, because a verdict about the
source does not change by node. Only `terminal` short-circuits: a `hold` or a
`retry_resource` on a dead item would leave a player with nothing to render and
no path forward, and the ladder is the only thing that can still produce a
picture.
