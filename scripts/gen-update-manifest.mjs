// Build Tauri's dynamic-JSON update manifest from a release's assets.
// CLI:  node gen-update-manifest.mjs <assetsDir> <version> <base-download-url> <out.json> [notes]
import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

// Match each Tauri target key to the regex that identifies its updater artifact
// (the bundle, NOT its .sig). createUpdaterArtifacts emits these names.
const TARGETS = [
	{ key: 'windows-x86_64', re: /_x64-setup\.exe$/ },
	{ key: 'linux-x86_64', re: /_amd64\.AppImage$/ },
	{ key: 'linux-aarch64', re: /_aarch64\.AppImage$/ },
	{ key: 'darwin-aarch64', re: /aarch64\.app\.tar\.gz$/ },
	{ key: 'darwin-x86_64', re: /x64\.app\.tar\.gz$/ }
];

// assets: { name -> sigContents|null }. Pure; unit-tested.
export function buildManifest({ version, notes, pubDate, assets, base }) {
	const names = Object.keys(assets);
	const platforms = {};
	for (const { key, re } of TARGETS) {
		const bundle = names.find((n) => re.test(n) && !n.endsWith('.sig'));
		if (!bundle) continue; // target not built this run
		const sig = assets[bundle + '.sig'];
		if (sig == null) throw new Error(`missing signature for ${bundle}`);
		platforms[key] = { url: `${base}/${bundle}`, signature: sig };
	}
	return { version, notes, pub_date: pubDate, platforms };
}

function main() {
	const [assetsDir, version, base, out, notes = ''] = process.argv.slice(2);
	if (!assetsDir || !version || !base || !out) {
		console.error('usage: gen-update-manifest.mjs <assetsDir> <version> <base-url> <out.json> [notes]');
		process.exit(2);
	}
	const files = readdirSync(assetsDir, { recursive: true }).filter((f) => typeof f === 'string');
	const assets = {};
	for (const f of files) {
		const name = f.split(/[\\/]/).pop();
		assets[name] = name.endsWith('.sig') ? readFileSync(join(assetsDir, f), 'utf8').trim() : null;
	}
	const manifest = buildManifest({
		version,
		notes,
		pubDate: new Date().toISOString(),
		assets,
		base
	});
	writeFileSync(out, JSON.stringify(manifest, null, 2));
	console.log(`wrote ${out} with platforms: ${Object.keys(manifest.platforms).join(', ')}`);
}

// Run main() only when invoked directly (not when imported by the test).
if (import.meta.url === `file://${process.argv[1]}`) main();
