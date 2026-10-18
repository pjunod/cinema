"""Authenticate two disjoint complete proofs with one unchanged decoding budget."""
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts
from validation.unit_evidence_objects import LiveLogs, Objects, StructuralBudget


def verify_bundle(api, raw, expected_digest, binding, candidate, attachment_id, attachment_uuid):
    manifest, _, repository, attested = retention.verify_identity(
        api, raw, expected_digest, binding, candidate, attachment_id, attachment_uuid)
    receipts.require(set(manifest) == {'version', 'repository', 'pr', 'branch', 'base_sha',
                                      'source_commit', 'proofs'}
                     and manifest['version'] == 3 and manifest['source_commit'] == candidate
                     and isinstance(manifest['proofs'], list) and len(manifest['proofs']) == 2,
                     'Unknown or incomplete retained evidence bundle')
    children, attachments, digests, tables = [], {attachment_id}, {expected_digest}, {}
    suites, commands = 0, 0
    for descriptor in manifest['proofs']:
        receipts.require(isinstance(descriptor, dict)
                         and set(descriptor) == {'sha256', 'attachment_id', 'attachment_uuid'}
                         and isinstance(descriptor['sha256'], str)
                         and retention.DIGEST.fullmatch(descriptor['sha256'])
                         and descriptor['sha256'] not in digests,
                         'Repeated or malformed bundle child digest')
        child_id = receipts.positive(descriptor['attachment_id'])
        receipts.require(child_id not in attachments, 'Repeated bundle attachment')
        attachments.add(child_id)
        digests.add(descriptor['sha256'])
        child_raw = retention.attachment_bytes(api, manifest['pr'], child_id,
            descriptor['attachment_uuid'], descriptor['sha256'])
        receipts.require(retention.digest(child_raw) == descriptor['sha256'],
                         'Bundle child digest differs')
        child = receipts.bounded_json(child_raw)
        receipts.require(child.get('version') == 2 and all(child.get(key) == manifest[key]
                         for key in ('repository', 'pr', 'branch', 'base_sha', 'source_commit')),
                         'Bundle child promotion identity differs or is recursive')
        lanes, units = child.get('lanes'), child.get('command_units')
        receipts.require(isinstance(lanes, dict) and lanes
                         and all(isinstance(slots, list) for slots in lanes.values())
                         and isinstance(units, dict) and units.get('version') == 2
                         and isinstance(units.get('commands'), list)
                         and isinstance(units.get('logs'), dict),
                         'Bundle children require structural evidence slots')
        slots = [slot for lane in lanes.values() for slot in lane] + units['commands']
        receipts.require(all(isinstance(slot, dict) and set(slot) == {'sha256_ref'}
                         and isinstance(slot['sha256_ref'], str)
                         and retention.DIGEST.fullmatch(slot['sha256_ref'])
                         and slot['sha256_ref'] in units['logs'] for slot in slots),
                         'Bundle structural reference is inline or outside its own child')
        suites += sum(len(lane) for lane in lanes.values())
        commands += len(units['commands'])
        receipts.require(suites <= 8 and commands <= 512,
                         'Bundle aggregate structural slots exceed bounds')
        for sha, blob in units['logs'].items():
            receipts.require(sha not in tables or tables[sha] == blob,
                             'Conflicting bundle evidence table entry')
            tables[sha] = blob
        receipts.require(0 < len(tables) <= 1024, 'Bundle aggregate evidence table exceeds bound')
        children.append((descriptor, child_raw, units['logs']))

    budget, live_logs = StructuralBudget(), LiveLogs()
    adopted, command_units, verified = {}, {}, []
    for descriptor, child_raw, logs in children:
        # Lookup stays child-local. Only byte/node counters and live CI logs are shared.
        proof = retention.verify(api, child_raw, descriptor['sha256'], binding, candidate,
            descriptor['attachment_id'], descriptor['attachment_uuid'],
            objects=Objects(logs, budget), live_logs=live_logs)
        receipts.require(not (set(adopted) & set(proof['lanes']))
                         and not (set(command_units) & set(proof.get('command_units', {}))),
                         'Overlapping bundle unit authority')
        adopted.update(proof['lanes'])
        command_units.update(proof.get('command_units', {}))
        receipts.require(not ('android_jvm' in adopted and 'android_jvm' in command_units),
                         'Multiple bundle authorities for JVM units')
        verified.append(proof)
    return {'version': 1, 'candidate_sha': candidate, 'base_sha': binding['base_sha'],
            'pull_request': manifest['pr'], 'sha256': expected_digest,
            'attestation_comments': sorted(attested), 'lanes': adopted,
            'attachment_id': attachment_id, 'attachment_uuid': attachment_uuid,
            'command_units': command_units, 'bundle_proofs': verified}
