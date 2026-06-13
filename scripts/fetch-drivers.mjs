// Fetch the Windows driver payloads Pulsar bundles + auto-installs:
//   • Interception   (keyboard capture under ASTER) — interception.dll + installer
//   • ViGEmBus       (virtual Xbox gamepad on the host)        — installer
//   • Virtual-Audio-Driver (sinkless render endpoint we redirect to so the host
//     can stream its audio while staying SILENT, Sunshine-style) — signed
//     inf/sys/cat, plus nefconw to create the device node + install the driver
//
// Mirrors scripts/fetch-ffmpeg.mjs: run once before `tauri build` (CI runs it
// automatically) so a dev clone needs no manual driver download. Files land in
// src-tauri/resources/ (git-ignored, except the small committed interception.dll
// fallback). No-op on non-Windows hosts.
//
// Licensing: Interception is LGPL-3.0 for non-commercial use with explicit
// redistribution rights for the driver + installer (fine for GPLv3 Pulsar).
// ViGEmBus is BSD-3-Clause. Virtual-Audio-Driver is MIT + MS-PL (signed by the
// SignPath Foundation); nefcon (NefCon) is BSD-3-Clause. All redistributable.
// See desktop-app/CLAUDE.md → "Windows drivers".

import { mkdir, writeFile, access, rm, readdir, copyFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { tmpdir } from 'node:os';

const here = dirname(fileURLToPath(import.meta.url));
const resDir = join(here, '..', 'src-tauri', 'resources');
const interceptionDir = join(resDir, 'interception');
const vadDir = join(resDir, 'virtual-audio-driver');
const nefconDir = join(resDir, 'nefcon');

// Pinned release assets (bump deliberately; all are signed / from signed sources).
const INTERCEPTION_ZIP =
	'https://github.com/oblitum/Interception/releases/download/v1.0.1/Interception.zip';
const VIGEMBUS_EXE =
	'https://github.com/ViGEm/ViGEmBus/releases/download/v1.22.0/ViGEmBus_1.22.0_x64_x86_arm64.exe';
// Virtual-Audio-Driver: latest SIGNED release (SignPath Foundation cert).
// Zip layout: "Virtual Audio Driver/{VirtualAudioDriver.inf,VirtualAudioDriver.sys,virtualaudiodriver.cat}"
// (filenames bundled verbatim so the signed .cat keeps matching the .sys/.inf).
const VAD_ZIP =
	'https://github.com/VirtualDrivers/Virtual-Audio-Driver/releases/download/25.7.14/Virtual.Audio.Driver.Signed.-.25.7.14.zip';
// nefcon (NefCon) — creates the ROOT\VirtualAudioDriver device node + installs the
// INF silently. Zip layout: "{x64,ARM64,x86}/nefconw.exe".
const NEFCON_ZIP =
	'https://github.com/nefarius/nefcon/releases/download/v1.17.40/nefcon_v1.17.40.zip';

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

// Some shells (e.g. Steam) leak env vars containing NUL bytes, which node's
// spawn rejects. Build a clean copy that drops any such values (mirrors
// scripts/fetch-ffmpeg.mjs).
function cleanEnv() {
	const out = {};
	for (const [k, v] of Object.entries(process.env)) {
		if (typeof v === 'string' && !v.includes('\x00')) out[k] = v;
	}
	return out;
}

function run(cmd, args) {
	return new Promise((resolve, reject) => {
		const p = spawn(cmd, args, { stdio: 'inherit', env: cleanEnv() });
		p.on('error', reject);
		p.on('exit', (code) =>
			code === 0 ? resolve() : reject(new Error(`${cmd} exited ${code}`))
		);
	});
}

// Extract a .zip into destDir. We're Windows-only here, so PowerShell's
// Expand-Archive (always present on Win10/11) is the dependency-free choice.
async function unzip(zip, destDir) {
	await mkdir(destDir, { recursive: true });
	await run('powershell', [
		'-NoProfile',
		'-NonInteractive',
		'-Command',
		`Expand-Archive -LiteralPath '${zip}' -DestinationPath '${destDir}' -Force`
	]);
}

// Pull the Virtual-Audio-Driver signed inf/sys/cat into resources/virtual-audio-driver/.
// The NSIS hook + the redirect code expect a FLAT folder of those three files.
async function fetchVirtualAudioDriver() {
	const inf = join(vadDir, 'VirtualAudioDriver.inf');
	if (await exists(inf)) return; // idempotent: already extracted.

	const work = join(tmpdir(), `pulsar-vad-${Date.now()}`);
	await mkdir(work, { recursive: true });
	try {
		const zip = join(work, 'vad.zip');
		await download(VAD_ZIP, zip);
		await unzip(zip, work);

		// Zip nests the payload under "Virtual Audio Driver/"; flatten it.
		const inner = join(work, 'Virtual Audio Driver');
		const srcDir = (await exists(inner)) ? inner : work;
		await mkdir(vadDir, { recursive: true });
		for (const f of await readdir(srcDir)) {
			if (/\.(inf|sys|cat)$/i.test(f)) {
				await copyFile(join(srcDir, f), join(vadDir, f));
				process.stdout.write(`  -> ${join(vadDir, f)}\n`);
			}
		}
		if (!(await exists(inf)))
			throw new Error('VirtualAudioDriver.inf not found in extracted zip');
	} finally {
		await rm(work, { recursive: true, force: true });
	}
}

// Pull nefconw.exe for x64 + ARM64 into resources/nefcon/{x64,arm64}/nefconw.exe.
async function fetchNefcon() {
	const x64 = join(nefconDir, 'x64', 'nefconw.exe');
	const arm64 = join(nefconDir, 'arm64', 'nefconw.exe');
	if ((await exists(x64)) && (await exists(arm64))) return; // idempotent.

	const work = join(tmpdir(), `pulsar-nefcon-${Date.now()}`);
	await mkdir(work, { recursive: true });
	try {
		const zip = join(work, 'nefcon.zip');
		await download(NEFCON_ZIP, zip);
		await unzip(zip, work);
		for (const [arch, dest] of [
			['x64', x64],
			['ARM64', arm64]
		]) {
			const src = join(work, arch, 'nefconw.exe');
			if (!(await exists(src)))
				throw new Error(`nefconw.exe not found for ${arch} in extracted zip`);
			await mkdir(dirname(dest), { recursive: true });
			await copyFile(src, dest);
			process.stdout.write(`  -> ${dest}\n`);
		}
	} finally {
		await rm(work, { recursive: true, force: true });
	}
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

	// Virtual sink for Sunshine-style host-silent audio (redirect default endpoint
	// here, capture ITS loopback, restore on teardown — never mute the real one).
	await fetchVirtualAudioDriver();
	await fetchNefcon();

	process.stdout.write('fetch-drivers: done.\n');
}

main().catch((e) => {
	console.error('fetch-drivers failed:', e.message);
	process.exit(1);
});
