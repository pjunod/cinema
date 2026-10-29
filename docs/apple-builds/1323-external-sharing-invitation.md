# Connect an external Cinema through one invitation

Build: 278
Issue: #1323

Follow-up to the private host setup: a Cinema invitation now carries the
Tailscale device consent step for someone outside the publisher’s tailnet.
Apple imports retain the transient invitation while consent is pending and
continue after pinned connectivity succeeds. Leaving, changing account or
editing the draft retires that attempt. Existing pairing approval remains
required before library access.

The publishing server authorizes its Tailscale account once in Sharing.
Serving nodes discover their own private addresses and Cinema certificate;
manual endpoint editing remains advanced recovery. Real cross-tailnet and
playback acceptance remains open in the [host setup ledger](../features/SHARING-HOST-SETUP.md).
