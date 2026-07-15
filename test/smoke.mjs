/**
 * Centinelo Phone — smoke test (no display needed, CI-safe).
 * 1. Syntax-checks every first-party JS file (main, preload, renderer, extension).
 * 2. Verifies the vendored SIP.js bundle exposes the expected globals.
 * 3. Sanity-checks package.json build config.
 * Full e2e (register + echo test against the PBX) runs on the LAN, not in CI.
 */
import { execFileSync } from 'node:child_process';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
let failures = 0;

function check(label, fn) {
  try {
    fn();
    console.log(`  ok   ${label}`);
  } catch (e) {
    failures++;
    console.error(`  FAIL ${label}: ${e.message.split('\n')[0]}`);
  }
}

function jsFiles(dir) {
  const out = [];
  for (const entry of readdirSync(dir)) {
    const p = join(dir, entry);
    if (statSync(p).isDirectory()) {
      if (entry === 'node_modules' || entry === 'vendor' || entry === 'dist') continue;
      out.push(...jsFiles(p));
    } else if (entry.endsWith('.js') || entry.endsWith('.mjs')) {
      out.push(p);
    }
  }
  return out;
}

console.log('Syntax check:');
for (const f of [...jsFiles(join(root, 'src')), ...jsFiles(join(root, 'extension')), ...jsFiles(join(root, 'test'))]) {
  check(f.replace(root + '/', ''), () => execFileSync(process.execPath, ['--check', f], { stdio: 'pipe' }));
}

console.log('Vendor bundle:');
check('sip-0.21.2.min.js exposes SIP globals', () => {
  const bundle = readFileSync(join(root, 'src', 'renderer', 'vendor', 'sip-0.21.2.min.js'), 'utf8');
  for (const sym of ['UserAgent', 'Registerer', 'Inviter', 'Subscriber', 'holdModifier']) {
    if (!bundle.includes(sym)) throw new Error(`missing symbol ${sym}`);
  }
});

console.log('Build config:');
check('package.json build block', () => {
  const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));
  if (pkg.main !== 'src/main/main.js') throw new Error('main entry mismatch');
  if (!pkg.build || pkg.build.appId !== 'com.centinelo.phone') throw new Error('appId mismatch');
  if (!pkg.build.win) throw new Error('missing win target');
});

check('renderer version matches package.json', () => {
  const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));
  const common = readFileSync(join(root, 'src', 'renderer', 'common.js'), 'utf8');
  if (!common.includes(`APP_VERSION: '${pkg.version}'`)) {
    throw new Error(`common.js APP_VERSION out of sync with ${pkg.version}`);
  }
});

if (failures) {
  console.error(`\n${failures} check(s) failed`);
  process.exit(1);
}
console.log('\nAll smoke checks passed.');
