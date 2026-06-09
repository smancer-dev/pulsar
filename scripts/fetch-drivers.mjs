// Fetch the Windows driver payloads Pulsar bundles + auto-installs:
//   • Interception (keyboard capture under ASTER) — interception.dll + installer
//   • ViGEmBus    (virtual Xbox gamepad on the host)        — installer
//
// Mirrors scripts/fetch-ffmpeg.mjs: run once before `tauri build` (CI runs it
// automatically) so a dev clone needs no manual driver download. Files land in
// src-tauri/resources/ (git-ignored, except the small committed interception.dll
// fallback). No-op on non-Windows hosts.
//
// Licensing: Interception is LGPL-3.0 for non-commercial use with explicit
// redistribution rights for the driver + installer (fine for GPLv3 Pulsar).
// ViGEmBus is BSD-3-Clause. See desktop-app/CLAUDE.md → "Windows drivers".

import { mkdir, writeFile, access } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const resDir = join(here, '..', 'src-tauri', 'resources');
const interceptionDir = join(resDir, 'interception');

// Pinned release assets (bump deliberately; both are signed installers).
const INTERCEPTION_ZIP =
	'https://github.com/oblitum/Interception/releases/download/v1.0.1/Interception.zip';
const VIGEMBUS_EXE =
	'https://github.com/ViGEm/ViGEmBus/releases/download/v1.22.0/ViGEmBus_1.22.0_x64_x86_arm64.exe';

async function exists(p) {
	try {
		await access(p);
		return true;
	} catch {
		return false;
	}
}

async function download(url, dest) {
	process.stdout.write(`fetch ${url}\n`);
	const res = await fetch(url, { redirect: 'follow' });
	if (!res.ok) throw new Error(`${res.status} ${res.statusText} for ${url}`);
	const buf = Buffer.from(await res.arrayBuffer());
	await mkdir(dirname(dest), { recursive: true });
	await writeFile(dest, buf);
	process.stdout.write(`  -> ${dest} (${buf.length} bytes)\n`);
}

async function main() {
	if (process.platform !== 'win32') {
		process.stdout.write('fetch-drivers: non-Windows host — skipping.\n');
		return;
	}
	await mkdir(interceptionDir, { recursive: true });

	// interception.dll ships committed as a fallback; only fetch the full release
	// (which carries the command-line installer the NSIS hook runs) if missing.
	const interceptionZip = join(interceptionDir, 'Interception.zip');
	if (!(await exists(interceptionZip))) {
		await download(INTERCEPTION_ZIP, interceptionZip);
		process.stdout.write(
			'  note: extract command-line-installer\\install-interception.exe + library\\x64\\interception.dll\n'
		);
	}

	const vigem = join(resDir, 'ViGEmBus_Setup.exe');
	if (!(await exists(vigem))) {
		await download(VIGEMBUS_EXE, vigem);
	}

	process.stdout.write('fetch-drivers: done.\n');
}

main().catch((e) => {
	console.error('fetch-drivers failed:', e.message);
	process.exit(1);
});
