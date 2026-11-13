"""The host census names exactly one dead owner: a job custody or a playback owner."""
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT=Path(__file__).resolve().parents[2]
SCRIPT=ROOT/'scripts/source-custody-source-census'
loader=importlib.machinery.SourceFileLoader('source_custody_source_census',str(SCRIPT))
spec=importlib.util.spec_from_loader(loader.name,loader)
helper=importlib.util.module_from_spec(spec)
loader.exec_module(helper)
NODE='11111111-1111-4111-8111-111111111111'
CUSTODY='22222222-2222-4222-8222-222222222222'
JOB='33333333-3333-4333-8333-333333333333'
BOOT='44444444-4444-4444-8444-444444444444'
CLAIM='55555555-5555-4555-8555-555555555555'
KERNEL='linux:'+Path('/proc/sys/kernel/random/boot_id').read_text().strip()
# The Rust consumer's ForegroundOwner fields, in full: the proof must carry
# exactly these, since it refuses unknown and missing fields alike.
FOREGROUND_OWNER={'custody_id','lifetime_id','session_incarnation','producer_attempt',
                  'node_id','boot_id','fence'}
JOB_OWNER={'custody_id','job_id','node_id','boot_id','claim_id','fence'}


def evidence(record):
    return {'version':2,'reader_model':'daemon_mediated_v1','kernel_instance':KERNEL,
            'activation_id':CLAIM,'node_id':NODE,'anchor_nonce':CLAIM,'anchor_device':1,
            'anchor_inode':2,'anchor_ctime':[3,4],'record':record}


def job_record():
    return {'kind':'background','token':{'custody_id':CUSTODY,'owner':{'kind':'job','token':{
        'job_id':JOB,'node_id':NODE,'boot_id':BOOT,'claim_id':CLAIM,'fence':3,'revision':4,
        'lease_expires_ms':7}}}}


def playback_record(owner_id=CUSTODY):
    return {'kind':'foreground','token':{'owner':{
        'owner_id':owner_id,'lifetime_id':'lifetime','session_incarnation':JOB,
        'producer_attempt':'1','node_id':NODE,'boot_id':BOOT,'file_id':91,
        'source_size':100,'source_mtime':1,'source_generation':'g','policy':'rolling_or_transfer'},
        'fence':1,'lease_expires_at_ms':9}}


class WitnessOwner(unittest.TestCase):
    def setUp(self):
        self.directory=tempfile.TemporaryDirectory()
        self.witnesses=Path(self.directory.name)

    def tearDown(self):
        self.directory.cleanup()

    def write(self,family,record):
        (self.witnesses/f'{family}-{CUSTODY}.json').write_text(json.dumps(evidence(record)))

    def test_a_job_witness_names_its_job_owner(self):
        self.write('background',job_record())
        family,_,owner,lease=helper.witness_owner(self.witnesses,CUSTODY)
        self.assertEqual(family,'background')
        self.assertEqual(set(owner),JOB_OWNER)
        self.assertEqual((owner['job_id'],owner['claim_id'],lease),(JOB,CLAIM,7))

    def test_a_playback_witness_names_its_exact_registration(self):
        self.write('foreground',playback_record())
        family,_,owner,lease=helper.witness_owner(self.witnesses,CUSTODY)
        self.assertEqual(family,'foreground')
        self.assertEqual(set(owner),FOREGROUND_OWNER)
        self.assertEqual((owner['custody_id'],owner['session_incarnation'],owner['fence'],lease),
                         (CUSTODY,JOB,1,9))

    def test_a_playback_witness_for_another_owner_refuses(self):
        self.write('foreground',playback_record(owner_id=JOB))
        with self.assertRaisesRegex(helper.Refused,'not this playback owner'):
            helper.witness_owner(self.witnesses,CUSTODY)

    def test_a_record_of_the_other_family_refuses(self):
        self.write('foreground',job_record())
        with self.assertRaisesRegex(helper.Refused,'another custody family'):
            helper.witness_owner(self.witnesses,CUSTODY)

    def test_no_witness_or_two_witnesses_refuse(self):
        with self.assertRaisesRegex(helper.Refused,'no single poisoned witness'):
            helper.witness_owner(self.witnesses,CUSTODY)
        self.write('background',job_record())
        self.write('foreground',playback_record())
        with self.assertRaisesRegex(helper.Refused,'no single poisoned witness'):
            helper.witness_owner(self.witnesses,CUSTODY)

    def test_a_witness_from_another_kernel_boot_refuses(self):
        stale=evidence(playback_record())
        stale['kernel_instance']='linux:'+BOOT
        (self.witnesses/f'foreground-{CUSTODY}.json').write_text(json.dumps(stale))
        with self.assertRaisesRegex(helper.Refused,'another kernel boot'):
            helper.witness_owner(self.witnesses,CUSTODY)


if __name__=='__main__':
    unittest.main()
