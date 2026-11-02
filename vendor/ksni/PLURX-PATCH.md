# Vendored ksni 0.3.6 — consistent D-Bus menu item IDs

This is the crates.io `ksni` 0.3.6 package, under its original
[Unlicense](UNLICENSE). [UPSTREAM-SHA256.json](UPSTREAM-SHA256.json) records
the original archive checksum and file hashes; registry-provenance.lock
preserves the registry identity for advisory scanning.

After a menu layout change, ksni offsets item IDs to invalidate old layouts.
`GetLayout`, item properties and click dispatch use that offset, but
`GetGroupProperties` with an empty ID list returned raw array indices.
Hosts fetching all menu properties could therefore receive IDs that every
subsequent click rejected as expired. The sole production source change is
`Service::get_all_item` calling `index2id(i)` instead of casting `i`.

`linux::tray::tests::dbus_menu_clicks_reach_the_shell_after_refresh` in the
tray crate exercises the real exported menu on a private session bus,
refreshes its layout and clicks the returned command IDs. It failed with
`id not found` against the original package. Remove this patch once a pinned
upstream release includes the equivalent correction.
