"""Post-boot retries cannot bypass readiness or restart completed stages."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
MODULE = ROOT / '.github/actions/android-emulator-ready/lib/startup-readiness.js'


class EmulatorStartupCase(unittest.TestCase):
    def simulate(self, body):
        script = '''const {prepareDevice} = require(process.argv[1]);
        let time = 0; const calls = [];
        const ready = (args) => args[1] === 'service'
            ? `Service ${args[3]}: found` : args[1] === 'cmd' ? 'package:/system/framework/framework-res.apk' : '';
        ''' + body
        result = subprocess.run(['node', '-e', script, str(MODULE)],
                                capture_output=True, text=True, timeout=5, check=True)
        return json.loads(result.stdout.splitlines()[-1])

    def test_missing_service_zero_exit_cannot_unlock(self):
        result = self.simulate('''
        prepareDevice(5554, {}, {now: () => time, wait: async ms => {time += ms;},
          run: async (port, args, limit) => {calls.push({args, limit}); time += 1000;
            return {ok: true, stdout: 'Service package: not found'};}
        }).then(() => {throw Error('accepted missing service');}, error =>
          console.log(JSON.stringify({calls, time, error: error.message})));
        ''')
        self.assertIn('exceeded 60 seconds', result['error'])
        self.assertTrue(all(call['args'][1:3] == ['service', 'check'] for call in result['calls']))

    def test_retry_resumes_failed_stage_without_repeating_unlock(self):
        result = self.simulate('''
        let failed = false;
        prepareDevice(5554, {disableAnimations: true}, {now: () => time, wait: async ms => {time += ms;},
          run: async (port, args, limit) => {calls.push({args, limit});
            if (args.includes('transition_animation_scale') && !failed) {
              failed = true; return {ok: false, stdout: '', diagnostic: 'device offline'};
            }
            return {ok: true, stdout: ready(args)};}
        }).then(() => console.log(JSON.stringify({calls, time})));
        ''')
        commands = [call['args'] for call in result['calls']]
        self.assertEqual(sum('keyevent' in args for args in commands), 1)
        self.assertEqual(sum('window_animation_scale' in args for args in commands), 1)
        self.assertEqual(sum('transition_animation_scale' in args for args in commands), 2)
        self.assertEqual(sum('animator_duration_scale' in args for args in commands), 1)

    def test_shared_deadline_bounds_every_child(self):
        result = self.simulate('''
        prepareDevice(5554, {}, {now: () => time, wait: async ms => {time += ms;},
          run: async (port, args, limit) => {calls.push({args, limit, before: time});
            time += limit; return {ok: false, stdout: '', diagnostic: 'child timeout'};}
        }).then(() => {throw Error('accepted deadline');}, error =>
          console.log(JSON.stringify({calls, time, error: error.message})));
        ''')
        self.assertEqual(result['time'], 60000)
        for call in result['calls']:
            self.assertLessEqual(call['limit'], min(10000, 60000 - call['before']))
            self.assertGreater(call['limit'], 0)

    def test_timed_out_adb_is_reaped_before_return(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / 'platform-tools/adb'
            binary.parent.mkdir()
            binary.write_text('#!/bin/sh\necho $$\nexec sleep 60\n')
            binary.chmod(0o755)
            environment = dict(os.environ, ANDROID_HOME=directory)
            script = 'require(process.argv[1]).runAdb(5554, ["get-state"], 100).then(r => console.log(JSON.stringify(r)))'
            result = subprocess.run(['node', '-e', script, str(MODULE)], env=environment,
                                    capture_output=True, text=True, timeout=5, check=True)
            outcome = json.loads(result.stdout)
            self.assertFalse(outcome['ok'])
            child = int(outcome['stdout'].strip())
            with self.assertRaises(ProcessLookupError):
                os.kill(child, 0)


if __name__ == '__main__':
    unittest.main()
