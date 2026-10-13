'use strict';
// Local post-boot preparation; the upstream instrumentation script is untouched.
const { execFile } = require('node:child_process');
const path = require('node:path');
const { performance } = require('node:perf_hooks');

function runAdb(port, args, timeoutMs) {
    const sdk = process.env.ANDROID_HOME;
    if (!sdk) throw new Error('ANDROID_HOME is required for exact SDK adb');
    const binary = path.join(sdk, 'platform-tools', 'adb');
    return new Promise((resolve) => {
        // execFile completes after the child is reaped. A timeout kills the
        // actual adb child before the caller can start another attempt.
        execFile(binary, ['-s', `emulator-${port}`, ...args], {
            timeout: timeoutMs, killSignal: 'SIGKILL', maxBuffer: 16384,
            encoding: 'utf8',
        }, (error, stdout, stderr) => resolve({
            ok: !error, stdout: stdout || '',
            diagnostic: String(stderr || error?.message || '').slice(-2048),
        }));
    });
}

async function prepareDevice(port, options, runtime = {}) {
    const now = runtime.now || (() => performance.now());
    const wait = runtime.wait || ((ms) => new Promise((resolve) => setTimeout(resolve, ms)));
    const run = runtime.run || runAdb;
    const deadline = now() + 60000;
    let last = 'no readiness response';
    async function stage(args, accept = () => true) {
        while (now() < deadline) {
            const remaining = Math.floor(deadline - now());
            if (remaining <= 0) break;
            const result = await run(port, args, Math.min(10000, remaining));
            if (now() <= deadline && result.ok && accept(result.stdout.trim())) return;
            last = `${args.join(' ')}: ${result.stdout || ''} ${result.diagnostic || ''}`.slice(-2048);
            const pause = Math.min(250, Math.max(0, deadline - now()));
            if (pause) await wait(pause);
        }
        throw new Error(`Post-boot preparation exceeded 60 seconds; last response: ${last}`);
    }
    // Exit zero alone is insufficient: Android returns it for missing services.
    await stage(['shell', 'service', 'check', 'package'], (value) => /^Service package: found\s*$/.test(value));
    await stage(['shell', 'service', 'check', 'input'], (value) => /^Service input: found\s*$/.test(value));
    await stage(['shell', 'cmd', 'package', 'path', 'android'], (value) => /^package:\//m.test(value));
    await stage(['shell', 'input', 'keyevent', '82']);
    if (options.disableAnimations) {
        for (const setting of ['window_animation_scale', 'transition_animation_scale', 'animator_duration_scale']) {
            await stage(['shell', 'settings', 'put', 'global', setting, '0.0']);
        }
    }
    if (options.disableSpellChecker) await stage(['shell', 'settings', 'put', 'secure', 'spell_checker_enabled', '0']);
    if (options.enableHardwareKeyboard) await stage(['shell', 'settings', 'put', 'secure', 'show_ime_with_hard_keyboard', '0']);
    console.log('Post-boot services, unlock and requested settings are ready.');
}
module.exports = { prepareDevice, runAdb };
