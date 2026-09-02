#!/usr/bin/env node
// fetch-player.mjs — download the native player (ffplay.exe) used by the
// high-performance native renderer. ffplay ships INSIDE the same BtbN GPL ffmpeg
// build we already use (fetch-ffmpeg.mjs), so this reuses that source. Windows only
// (the native renderer is Windows/Interception-gated for now).
//
// Output: desktop-app/src-tauri/resources/ffplay.exe

import { createWriteStream, existsSync } from 'node:fs';
import { mkdir, mkdtemp, rm, rename, stat, readdir, copyFile, chmod } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { pipeline } from 'node:stream/promises';
import { Readable } from 'node:stream';

const RES = join(fileURLToPath(new URL('.', import.meta.url)), '..', 'src-tauri', 'resources');
// ffplay is the FALLBACK video renderer: on Windows when pulsar-render is missing, on
// macOS when mpv is not installed (play.rs spawns it with software decode so a fresh
// Mac still shows video with nothing installed). Same --os/--arch selection as
// fetch-ffmpeg.mjs; Linux never bundles it (mpv / pulsar-render are the players).
function arg(name) {
	const i = process.argv.indexOf(`--${name}`);
	return i !== -1 ? process.argv[i + 1] : undefined;
}
function detectOs() {
	const o = (arg('os') || process.env.FFMPEG_TARGET_OS || '').toLowerCase();
	if (o) return o;
	const r = (process.env.RUNNER_OS || '').toLowerCase();
	if (r) return r === 'macos' ? 'macos' : r === 'windows' ? 'windows' : 'linux';
	return process.platform === 'win32' ? 'windows' : process.platform === 'darwin' ? 'macos' : 'linux';
}
function detectArch() {
	const a = (arg('arch') || process.env.FFMPEG_TARGET_ARCH || '').toLowerCase();
	if (a) return a === 'aarch64' ? 'arm64' : a;
	const r = (process.env.RUNNER_ARCH || '').toLowerCase();
	if (r) return r === 'arm64' ? 'arm64' : 'x64';
	return process.arch === 'arm64' ? 'arm64' : 'x64';
}
const OS = detectOs();
const ARCH = detectArch();
const BTBN = 'https://github.com/BtbN/FFmpeg-Builds/releases/download/latest';
const SOURCES = {
	'windows/x64': { url: `${BTBN}/ffmpeg-master-latest-win64-gpl.zip`, member: /\/bin\/ffplay\.exe$/i, out: 'ffplay.exe' },
	'windows/arm64': { url: `${BTBN}/ffmpeg-master-latest-winarm64-gpl.zip`, member: /\/bin\/ffplay\.exe$/i, out: 'ffplay.exe' },
	// evermeet.cx (Intel) / Martin Riedl (Apple Silicon): single ffplay binary in the zip,
	// same sources fetch-ffmpeg.mjs uses for ffmpeg.
	'macos/x64': { url: 'https://evermeet.cx/ffmpeg/getrelease/ffplay/zip', member: /(^|\/)ffplay$/, out: 'ffplay' },
	'macos/arm64': { url: 'https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/ffplay.zip', member: /(^|\/)ffplay$/, out: 'ffplay' }
};

function run(cmd, args) {
	return new Promise((res, rej) => {
		const p = spawn(cmd, args, { stdio: 'inherit' });
		p.on('error', rej);
		p.on('close', (c) => (c === 0 ? res() : rej(new Error(`${cmd} exited ${c}`))));
	});
}

async function findFile(dir, re) {
	for (const e of await readdir(dir, { withFileTypes: true })) {
		const full = join(dir, e.name);
		if (e.isDirectory()) {
			const hit = await findFile(full, re);
			if (hit) return hit;
		} else if (re.test(full.replace(/\\/g, '/'))) {
			return full;
		}
	}
	return null;
}

async function main() {
	const src = SOURCES[`${OS}/${ARCH}`];
	if (!src) {
		console.log(`fetch-player: no bundled ffplay for ${OS}/${ARCH} — skipping.`);
		return;
	}
	await mkdir(RES, { recursive: true });
	const work = await mkdtemp(join(tmpdir(), 'pulsar-ffplay-'));
	const zip = join(work, 'f.zip');
	try {
		console.log('downloading ffmpeg build (for ffplay)...');
		const r = await fetch(src.url, { redirect: 'follow' });
		if (!r.ok || !r.body) throw new Error(`download ${r.status} ${r.statusText}`);
		await pipeline(Readable.fromWeb(r.body), createWriteStream(zip));
		const out = join(work, 'out');
		await mkdir(out, { recursive: true });
		console.log('extracting...');
		if (process.platform === 'win32') {
			await run('powershell', [
				'-NoProfile',
				'-Command',
				`Expand-Archive -LiteralPath '${zip}' -DestinationPath '${out}' -Force`
			]);
		} else {
			await run('unzip', ['-q', '-o', zip, '-d', out]);
		}
		const fp = await findFile(out, src.member);
		if (!fp) throw new Error(`${src.out} not found in archive`);
		const dest = join(RES, src.out);
		if (existsSync(dest)) await rm(dest, { force: true });
		await rename(fp, dest).catch(() => copyFile(fp, dest));
		if (process.platform !== 'win32') await chmod(dest, 0o755);
		const s = await stat(dest);
		console.log(`Wrote ${dest} (${(s.size / 1e6).toFixed(1)} MB)`);
	} finally {
		await rm(work, { recursive: true, force: true });
	}
}

main().catch((e) => {
	console.error(e.message || e);
	process.exit(1);
});
