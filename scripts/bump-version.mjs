#!/usr/bin/env node
// bump-version.mjs — stamp a new version into every manifest that carries one.
// Called by the release workflow to stamp the version semantic-release computed
// into the build artifacts (the change is NOT committed — semantic-release is the
// source of truth via git tags):
//     node scripts/bump-version.mjs <new-version>
// Run from the app repo root. Keeps tab indentation so diffs stay clean.

import { readFileSync, writeFileSync } from 'node:fs';

const version = process.argv[2];
if (!version) {
	console.error('usage: bump-version.mjs <version>');
	process.exit(1);
}

// --- JSON manifests (preserve tab indentation + trailing newline) ---
for (const file of ['package.json', 'src-tauri/tauri.conf.json']) {
	const json = JSON.parse(readFileSync(file, 'utf8'));
	json.version = version;
	writeFileSync(file, JSON.stringify(json, null, '\t') + '\n');
	console.log(`  ${file} -> ${version}`);
}

// --- Cargo workspace version (pulsar-core / -tauri / -mobile / … inherit it via
//     `version.workspace = true`, so only the root [workspace.package] edits) ---
const cargoFile = 'Cargo.toml';
const cargo = readFileSync(cargoFile, 'utf8');
const cargoRe = /(\[workspace\.package\][\s\S]*?\nversion\s*=\s*")[^"]*(")/;
if (!cargoRe.test(cargo)) {
	console.error('Cargo.toml: could not find [workspace.package] version to bump');
	process.exit(1);
}
writeFileSync(cargoFile, cargo.replace(cargoRe, `$1${version}$2`));
console.log(`  ${cargoFile} -> ${version}`);
