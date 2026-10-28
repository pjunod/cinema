# Dolby Vision title scanning — metadata, shared reads and demand identity

**Status:** open · Implementation reviewed in [PR #1301](http://forge.lan:3000/noirr/plurx/pulls/1301); qualification and landing tracked there. Not deployed.

Companion to [the display-targeted DV build ledger](DISPLAY-TARGETED-DV-BUILD.md)
(target selection and rendering) — this is how the title scanner reads and
stores the facts that select a target. Paul deploys plurxd, which rebuilds the
DV bundle; avoid mixed-version nodes during rollout.

## The scanner follows the renderer’s picture rules

The scan reads metadata, never pixels. `tools/dv_processing/scan_title.c` parses each access unit's RPU
straight from the demuxed bitstream. Until 2026-10-27 it decoded every picture
in software on one thread: hours of one core per title, with 835 titles queued
on lab5. Each access unit is judged by the rules the renderer's pipeline
applies (`dovi_split` `bl_rpu`, then the HEVC decoder), so the counts describe
the pictures the renderer will see:

- NALs are split as `ff_h2645_packet_split` splits them. Input must be
  length-prefixed; every catalogued DV title is Matroska or MP4.
- Enhancement-layer NALs (UNSPEC63) never reach the decoder.
- A packet with a base-layer VCL NAL of a decodable type is a picture.
- RASL pictures after an IRAP with NoRaslOutputFlag are not output.
- A picture's RPU is the last UNSPEC62 NAL after the first one, with layer 0,
  temporal id 0 and more than its two header bytes. An RPU in a packet that
  produced no picture carries to the next picture.

Where the decoder would refuse a picture or withhold pictures from output, the
scan refuses, as the decoding scanner did. That covers a missing or repeated
first slice, an unknown PPS, a picture over 4096×2160, pictures discarded at a
new sequence (`no_output_of_prior_pics_flag`, forced for a CRA after an EOS),
and a PPS with `output_flag_present_flag`. The one accepted difference: the
decoder drops an RPU its own parser rejects, and the scanner can only ask
libdovi. The renderer still refuses any picture that arrives without a fresh
RPU. Parity qualification of the revised scanner against published P5, P7 FEL
and P8 reference aggregates is tracked on [PR #1301](http://forge.lan:3000/noirr/plurx/pulls/1301).

## Standalone and content analysis publish the same aggregate

It runs two ways, with one aggregate:

- **Standalone:** the `dv_title_scan` job reads its own open of the source. It
  also qualifies a Profile 7 FEL title while it holds it.
- **With content analysis:** while the fragment-index pass reads the file, the
  same scanner, in the same mode, reads a duplicate of the pass's held source.
  It sees the same unfiltered access units and runs the same checks, and the
  two read the same pages in step, so the page cache serves the slower one.
  The scan never touches the pass's argv, descriptors or exit status. It is
  kept only when the pass's lead output built, and killed otherwise, including
  when the pass future is dropped. The aggregate is stored through the same
  `publish_dv_title` command a job uses, under the title's current demand
  generation, with no job attached:
  - an unbound demand is bound to the pass's scanner first;
  - a demand another scanner's job is working on is left to that job.

  The demand stays open; the title job finds that generation's aggregate
  stored and settles without reading the file (and qualifies a FEL title). A
  fresh title's job waits up to an hour for the node-local pass that reads it
  anyway.

## Scanner identity keeps renderer updates from re-queuing titles

Title demand is keyed by the scanner executable alone
(`ToolSet::title_scanner_digest`; the scanner links ffmpeg and libdovi
statically, so its digest covers its parser). It used to be the whole DV
backend (renderer, libraries, parser, every helper source), so any renderer,
shader or libplacebo change re-queued every title. FEL verdicts keep the
backend key (`backend_digest`), because they measure the renderer. Since a
renderer change no longer re-queues titles, the title worker re-qualifies
them explicitly instead: on an idle tick it asks again, throttled per title,
for settled FEL titles whose verdict another FEL identity measured. On the
first deploy every demand rebinds once to the new key, so titles published
under the old key are rescanned.
