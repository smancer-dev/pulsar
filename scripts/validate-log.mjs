#!/usr/bin/env bun
// Adaptive-streaming validation (Phase 4, the real-session half): read a Pulsar desktop
// session log (the daily log file — Settings → General → "Open log folder" — or a `bun run
// tauri dev` terminal capture) and assert the design doc's acceptance criteria from what the
// client/host/renderer logged:
//
//   * renderer loss holds  — `renderer loss hold … end=keyframe|deadline frames=N ms=M`
//                            (a freeze > --max-hold ms fails)
//   * controller windows   — `abr window` / `abr step` / `abr fast step` (loss, rtt, excess,
//                            delay state, target, point, ceiling, reason)
//   * host feedback        — `client stats` (what the host received + its fec group size)
//   * stalls               — `play-stall` / stalled markers, session teardowns
//
// Usage:
//   bun scripts/validate-log.mjs <logfile> [--max-hold 300] [--min-steady 120] [--expect-point 720p30]
//   bun scripts/validate-log.mjs ~/.local/share/com.pulsardesk.app/logs/pulsar.2026-09-03.log
//
// Exit code 1 when a criterion fails; the report is printed either way.

import { readFileSync } from 'node:fs';

const args = process.argv.slice(2);
if (args.length === 0 || args.includes('-h') || args.includes('--help')) {
	console.log('usage: validate-log.mjs <logfile> [--max-hold MS] [--min-steady SECONDS] [--expect-point LABEL] [--json]');
	process.exit(args.length === 0 ? 2 : 0);
}
const opt = (name, dflt) => {
	const i = args.indexOf(name);
	return i >= 0 && args[i + 1] !== undefined ? args[i + 1] : dflt;
};
const file = args.find((a) => !a.startsWith('--') && a !== opt('--max-hold') && a !== opt('--min-steady') && a !== opt('--expect-point'));
const maxHoldMs = Number(opt('--max-hold', 300));
const minSteadyS = Number(opt('--min-steady', 120));
const expectPoint = opt('--expect-point', null);
const asJson = args.includes('--json');

const text = readFileSync(file, 'utf8');
const lines = text.split(/\r?\n/);

// tracing's default text format: `2026-09-03T10:00:00.123456Z  INFO target: message k=v k2=v2`.
// Values may be quoted, `Debug`-formatted (`recovery=IntraRefresh`) or bare numbers.
const kv = (line) => {
	const out = {};
	for (const m of line.matchAll(/([a-zA-Z_][a-zA-Z0-9_]*)=("([^"]*)"|[^\s]+)/g)) {
		out[m[1]] = m[3] !== undefined ? m[3] : m[2];
	}
	return out;
};
const tsOf = (line) => {
	const m = line.match(/^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z?)/);
	return m ? Date.parse(m[1].endsWith('Z') ? m[1] : m[1] + 'Z') : NaN;
};

const holds = [];
const windows = [];
const steps = [];
const fast = [];
const hostStats = [];
const stalls = [];
const ended = [];
let firstTs = NaN;
let lastTs = NaN;
for (const line of lines) {
	const ts = tsOf(line);
	if (!Number.isNaN(ts)) {
		if (Number.isNaN(firstTs)) firstTs = ts;
		lastTs = ts;
	}
	if (line.includes('renderer loss hold')) {
		const k = kv(line);
		holds.push({ ts, end: k.end, frames: Number(k.frames), ms: Number(k.ms) });
	} else if (line.includes('abr window')) {
		windows.push({ ts, ...kv(line) });
	} else if (line.includes('abr fast step')) {
		fast.push({ ts, ...kv(line) });
	} else if (line.includes('abr step')) {
		steps.push({ ts, ...kv(line) });
	} else if (line.includes('client stats')) {
		hostStats.push({ ts, ...kv(line) });
	} else if (/stall|STALLED/i.test(line) && !/stall=false|unstall/i.test(line)) {
		stalls.push({ ts, line: line.trim() });
	} else if (line.includes('play-ended') || line.includes('ending play session') || line.includes('host silent')) {
		ended.push({ ts, line: line.trim() });
	}
}

const num = (v) => (v === undefined ? NaN : Number(v));
const durationS = Number.isNaN(firstTs) || Number.isNaN(lastTs) ? 0 : (lastTs - firstTs) / 1000;

// ── Criteria ───────────────────────────────────────────────────────────────────────────
const failures = [];
const notes = [];

const longHolds = holds.filter((h) => h.ms > maxHoldMs);
if (longHolds.length) failures.push(`${longHolds.length} loss hold(s) longer than ${maxHoldMs} ms (max ${Math.max(...longHolds.map((h) => h.ms))} ms)`);
const deadlineHolds = holds.filter((h) => h.end === 'deadline').length;
notes.push(`loss holds: ${holds.length} (keyframe-ended ${holds.length - deadlineHolds}, deadline-ended ${deadlineHolds}, max ${holds.length ? Math.max(...holds.map((h) => h.ms)) : 0} ms)`);

if (stalls.length) failures.push(`${stalls.length} stall marker(s) in the log`);

// Steady state: after the first `minSteadyS` seconds, count steps and point changes.
const t0 = Number.isNaN(firstTs) ? 0 : firstTs + minSteadyS * 1000;
const lateSteps = steps.filter((s) => s.ts >= t0);
const latePointChanges = new Set();
let prevPoint = null;
for (const s of [...windows, ...steps].sort((a, b) => a.ts - b.ts)) {
	if (s.point && s.point !== prevPoint) {
		if (s.ts >= t0 && prevPoint !== null) latePointChanges.add(`${s.point}@${new Date(s.ts).toISOString()}`);
		prevPoint = s.point;
	}
}
const downLate = lateSteps.filter((s) => /halve|×0\.7|×0\.85|probe down|step down/.test(s.reason ?? ''));
if (durationS > minSteadyS + 60 && downLate.length > Math.max(3, durationS / 120)) {
	failures.push(`${downLate.length} down-steps after the first ${minSteadyS} s (sawtooth?)`);
}
if (durationS > minSteadyS + 60 && latePointChanges.size > 3) {
	failures.push(`${latePointChanges.size} operating-point changes after the first ${minSteadyS} s (oscillation?)`);
}
const lastPoint = [...windows, ...steps].sort((a, b) => a.ts - b.ts).at(-1)?.point ?? null;
if (expectPoint && lastPoint && lastPoint !== expectPoint) failures.push(`final point ${lastPoint}, expected ${expectPoint}`);

// Recovery mode flipped after the first lossy window?
const firstLossy = windows.concat(steps).sort((a, b) => a.ts - b.ts).find((w) => num(w.loss_pct) > 0.5);
// The flip rides the first step whose `recovery` is no longer Normal (its reason may be the
// rate reason of the same window, e.g. "severe loss → halve").
const flip = steps.find((s) => /recovery mode/.test(s.reason ?? '') || (s.recovery && s.recovery !== 'Normal'));
if (firstLossy && !flip) failures.push('loss seen but the recovery mode never flipped');
if (firstLossy && flip && flip.ts - firstLossy.ts > 6000) failures.push(`recovery flipped ${((flip.ts - firstLossy.ts) / 1000).toFixed(1)} s after the first lossy window`);

// Summaries.
const all = [...windows, ...steps].sort((a, b) => a.ts - b.ts);
const lossVals = all.map((w) => num(w.loss_pct)).filter((v) => !Number.isNaN(v));
const rttVals = all.map((w) => num(w.rtt)).filter((v) => !Number.isNaN(v));
const kbpsVals = all.map((w) => num(w.kbps)).filter((v) => !Number.isNaN(v));
const avg = (a) => (a.length ? a.reduce((x, y) => x + y, 0) / a.length : NaN);
const pct = (a, p) => (a.length ? [...a].sort((x, y) => x - y)[Math.min(a.length - 1, Math.floor(a.length * p))] : NaN);

const report = {
	file,
	duration_s: Math.round(durationS),
	windows: windows.length,
	steps: steps.length,
	fast_steps: fast.length,
	holds: holds.length,
	holds_over_max: longHolds.length,
	stalls: stalls.length,
	session_ends: ended.length,
	loss_pct: { avg: avg(lossVals), p95: pct(lossVals, 0.95) },
	rtt_ms: { avg: avg(rttVals), p95: pct(rttVals, 0.95) },
	kbps: { min: Math.min(...kbpsVals), max: Math.max(...kbpsVals), last: kbpsVals.at(-1) },
	final_point: lastPoint,
	recovery_flip: flip ? flip.recovery ?? 'yes' : null,
	host_stats_lines: hostStats.length,
	host_fec_n_last: hostStats.at(-1)?.fec_n ?? null,
	reasons: Object.fromEntries(
		Object.entries(
			[...steps, ...fast].reduce((m, s) => ((m[s.reason ?? '?'] = (m[s.reason ?? '?'] ?? 0) + 1), m), {}),
		).sort((a, b) => b[1] - a[1]),
	),
	failures,
	notes,
};

if (asJson) {
	console.log(JSON.stringify(report, null, 2));
} else {
	console.log(`# ${file}`);
	console.log(`duration ${report.duration_s}s · windows ${report.windows} · steps ${report.steps} (+${report.fast_steps} fast) · holds ${report.holds} · stalls ${report.stalls}`);
	console.log(`loss avg ${report.loss_pct.avg?.toFixed(2)}% p95 ${report.loss_pct.p95?.toFixed(2)}% · rtt avg ${report.rtt_ms.avg?.toFixed(0)} p95 ${report.rtt_ms.p95?.toFixed(0)} ms`);
	console.log(`kbps ${report.kbps.min}–${report.kbps.max} (last ${report.kbps.last}) · point ${report.final_point ?? '-'} · recovery ${report.recovery_flip ?? '-'} · host fec_n ${report.host_fec_n_last ?? '-'}`);
	for (const n of notes) console.log(`  ${n}`);
	console.log('step reasons:');
	for (const [r, c] of Object.entries(report.reasons)) console.log(`  ${String(c).padStart(4)}  ${r}`);
	if (failures.length) {
		console.log('\nFAIL');
		for (const f of failures) console.log(`  ✗ ${f}`);
	} else {
		console.log('\nPASS');
	}
}
process.exit(failures.length ? 1 : 0);
