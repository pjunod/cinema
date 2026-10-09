# Show verified Dolby Vision processing during playback

Build: 221
Issue: #968

The delivered-format label shows **HDR10-E** when the server reports supported
Dolby Vision processing for the current playback. Playback information keeps
FEL contribution separate from base/RPU processing. A Dolby Vision source tag
alone does not award the enhanced label.

Accepted control responses update the processing report. Seeking, replacement
and a response without a current report clear the earlier claim. Profile 8.1
output keeps its Dolby Vision delivery label; HDR10-E describes processed
HDR10 output.

The processing preferences are independent, default-off Developer settings.
The server retains compatible ordinary playback when processing cannot run.
Read the [implementation and acceptance ledger](../streaming/DV_HDR_PROCESSING_STATUS.md)
for the supported subset, measured resource limits and remaining physical
acceptance. This build note records source behavior; it does not claim a
TestFlight upload or device installation.
