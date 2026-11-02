# Connect and play through a server's generated HTTPS identity

Build: 286
Issue: #1352

Cinema remembers a selected server's self-signed HTTPS certificate on first
connection. API calls, artwork, playback control and native playback reuse that
identity. A changed, expired or invalid certificate is refused; ordinary retries
and sign-out do not replace the remembered certificate. Publicly trusted servers
keep normal system validation.

HTTP discovery addresses are resolved without credentials before sign-in or
saved-session recovery. The verified HTTPS origin becomes the saved API and
media address, including clients whose redirect policy intentionally refuses
later redirects. Passwords and saved bearers are not used for this probe.

Native HLS fragment requests bypass AVFoundation's asset authentication
callback. Private-certificate playback therefore uses an app-private loopback
bridge whose upstream requests use the same certificate policy as the API.
The listener binds only loopback, requires a random capability, preserves byte
ranges and rewrites playlist resources. Public-certificate and plaintext
playback retain their existing transport.

Background offline-video downloads retain remote URLs and require system-trusted
HTTPS or HTTP. Private-certificate video downloads are refused before creating a
background task; an app-local URL cannot survive suspension or relaunch.

First use trusts the selected network route; it does not independently prove
the server's identity. Subsequent connections compare the saved certificate.
If an operator intentionally replaces it, the connection screen offers explicit
replacement recovery and discards the previous login before enrollment.

Certificate policy and playlist regressions run in XCTest. Native self-signed
HLS was also exercised against a real TLS fixture. These checks do not claim
physical Apple TV acceptance or an installed device update.
