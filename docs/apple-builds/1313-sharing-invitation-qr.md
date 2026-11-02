# Exchange Cinema sharing invitations with QR images

Build: 280
Issue: #1313

Sharing presents invitations as QR codes with expandable text. iPhone and
iPad can save or share a PNG, upload an invitation image from Files or Photos,
and scan it with the camera where supported. A scan fills the current draft;
import and pairing approval remain explicit. Late results cannot replace a
new edit or restore a retired account draft. Oversized invitations retain a
text fallback. TV clients show the QR and offer text entry.

Validated with iOS and tvOS compilation plus focused sharing-management
regressions, including an actual saved PNG round trip. Physical camera and
share-sheet acceptance remain to be checked on a device.
