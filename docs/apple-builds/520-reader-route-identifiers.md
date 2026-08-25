# Preserve native reader route identifiers

Build: 80
Issue: #520

Native reader item and file identifiers are quoted before the JavaScript
handoff, preserving the full signed 64-bit route without putting the bearer in
the URL.
