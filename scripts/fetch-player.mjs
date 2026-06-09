#!/usr/bin/env node
// fetch-player.mjs — download the native player (ffplay.exe) used by the
// high-performance native renderer. ffplay ships INSIDE the same BtbN GPL ffmpeg
// build we already use (fetch-ffmpeg.mjs), so this reuses that source. Windows only
// (the native renderer is Windows/Interception-gated for now).
//
// Output: desktop-app/src-tauri/resources/ffplay.exe

import { createWriteStream, existsSync } from 'node:fs';
import { mkdir, mkdtemp, rm, rename, stat, readdir, copyFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { pipeline } from 'node:stream/promises';
import { Readable } from 'node:stream';

const RES = join(fileURLToPath(new URL('.', import.meta.url)), '..', 'src-tauri', 'resources');
const URL_WIN = 'https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip';

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
	if (process.platform !== 'win32' && !process.argv.includes('--force')) {
		console.log('fetch-player: native renderer is Windows-only — skipping.');
		return;
	}
	await mkdir(RES, { recursive: true });
	const work = await mkdtemp(join(tmpdir(), 'pulsar-ffplay-'));
	const zip = join(work, 'f.zip');
	try {
		console.log('downloading ffmpeg build (for ffplay)...');
		const r = await fetch(URL_WIN, { redirect: 'follow' });
		if (!r.ok || !r.body) throw new Error(`download ${r.status} ${r.statusText}`);
		await pipeline(Readable.fromWeb(r.body), createWriteStream(zip));
		const out = join(work, 'out');
		await mkdir(out, { recursive: true });
		console.log('extracting...');
		await run('powershell', [
			'-NoProfile',
			'-Command',
			`Expand-Archive -LiteralPath '${zip}' -DestinationPath '${out}' -Force`
		]);
		const fp = await findFile(out, /\/bin\/ffplay\.exe$/i);
		if (!fp) throw new Error('ffplay.exe not found in archive');
		const dest = join(RES, 'ffplay.exe');
		if (existsSync(dest)) await rm(dest, { force: true });
		await rename(fp, dest).catch(() => copyFile(fp, dest));
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
