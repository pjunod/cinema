#!/usr/bin/env python3
"""Generate exact-source counterfactual patches; never edit the source tree.

Apply one patch only in an owned throwaway clone, compile, copy the binary,
then restore the source. These are measurement controls, not product options.
"""
import argparse
import difflib
from pathlib import Path

root = Path(__file__).resolve().parents[4]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('control', choices=['independent-probe', 'per-frame-roundtrip'])
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()


def replace_once(text, old, new):
    if text.count(old) != 1:
        raise SystemExit('current-source control anchor is not unique')
    return text.replace(old, new, 1)


if args.control == 'independent-probe':
    filename = 'crates/plurxd/src/decode_facts.rs'
    before = (root / filename).read_text()
    old = '''        let selected_stream = prepared.key.selected_stream;
        source.seed = Some(FreshFactSeed {
            facts: prepared.facts,
            key: prepared.key,
        });
        self.get_or_probe(probe, source, catalog, selected_stream, budget, cancelled)
            .await'''
    new = '''        let selected_stream = prepared.key.selected_stream;
        // Measurement-only counterfactual: independent verification/planning
        // owners cannot share the just-collected document or cache entry.
        source.seed = None;
        let _verification_facts = prepared.facts;
        self.get_or_probe(probe, source, catalog, selected_stream, budget, cancelled)
            .await'''
    after = replace_once(before, old, new)
    after = replace_once(after, """        collected.key = Some(key.clone());
        let mut entries = self.entries.lock().await;""", """        collected.key = Some(key.clone());
        // The old verification owner did not populate the decoder cache.
        // Decoder observations still populate it, preserving warm-start hits.
        if source.projection == ProbeProjection::SourceDocument {
            return Ok((collected, DecodeFactLookupResult::MissCollected));
        }
        let mut entries = self.entries.lock().await;""")
else:
    filename = 'crates/plurxd/src/http/hls/response.rs'
    before = (root / filename).read_text()
    after = replace_once(before, '''struct AcceptedPrefix {
    pieces: Vec<(usize, tokio::time::Instant)>,
}''', '''struct AcceptedPrefix {
    pieces: Vec<(usize, tokio::time::Instant)>,
    // Measurement-only handshake; storage and delivery units are unchanged.
    reconciled: usize,
    reconciled_changed: tokio::sync::Notify,
}''')
    # A separate shared Notify avoids holding the acceptance mutex while waiting.
    after = after.replace('reconciled_changed: tokio::sync::Notify,',
                          'reconciled_changed: Arc<tokio::sync::Notify>,')
    after = replace_once(after, '''                    let prefix = accepted
                        .lock()''', '''                    let mut prefix = accepted
                        .lock()''')
    after = replace_once(after, '''                    reconciled = prefix.pieces.len();
                    if delivered == len''', '''                    reconciled = prefix.pieces.len();
                    prefix.reconciled = reconciled;
                    prefix.reconciled_changed.notify_one();
                    if delivered == len''')
    after = replace_once(after, '''            if let Some(current) = &mut batch {
                let mut prefix = current''', '''            if let Some(current) = &mut batch {
                // Counterfactual only: require the existing owner to reconcile
                // each 4 KiB acceptance before the next body frame can run.
                loop {
                    let (waiting, changed, deadline) = {
                        let prefix = current.accepted.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let last = prefix.pieces.last()
                            .map_or(current.progress_started, |(_, at)| *at);
                        (prefix.pieces.len() > prefix.reconciled,
                         Arc::clone(&prefix.reconciled_changed),
                         (last + MEDIA_BODY_NO_PROGRESS_TIMEOUT).min(body_deadline))
                    };
                    if !waiting { break; }
                    tokio::select! {
                        biased;
                        () = terminal.signal.cancelled() => break,
                        _ = tokio::time::sleep_until(deadline) => break,
                        () = changed.notified() => {}
                    }
                }
                let mut prefix = current''')

patch = ''.join(difflib.unified_diff(before.splitlines(True), after.splitlines(True),
                                   fromfile='a/' + filename, tofile='b/' + filename))
args.output.parent.mkdir(parents=True, exist_ok=True)
args.output.write_text(patch)
print(args.control + ': generated measurement-only patch')
