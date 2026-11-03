# Keep settings category input in its remote adapter

Build: 287
Issue: #1355

The settings category rail routes Right through the existing remote-adapter
layer before requesting entry into its selected category. Category selection,
content-entry requests and Back behavior are unchanged. This repairs the
input-fence violation inherited by the release-readiness change.

The source fence and compilation qualify placement and types; the existing
`testSettingsPickerHelpReportAndExactReturn` native journey covers Right into
Playback's Quality setting. Physical-device acceptance is not claimed here.
