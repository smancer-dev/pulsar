// Shared connect-target parsing/validation. A target is either a 9-digit relay ID
// (grouped in threes for display) or an IP / IP:port (has a '.' or ':'). Extracted
// from Home.svelte so the remote connect screen and the gaming-mode home share one
// source of truth (and so the on-screen numpad edits the same canonical form).

/** A target carrying a '.' or ':' is an address (IP / IP:port); otherwise a relay ID. */
export const isAddr = (v: string): boolean => /[.:]/.test(v);

/** Canonical input form: addresses keep only digits/dots/colons (max 21 chars,
 * covers IPv4:port); relay IDs are stripped to digits, capped at 9, and grouped
 * in threes ("641 724 395"). */
export const fmtTarget = (v: string): string =>
	isAddr(v)
		? v.replace(/[^0-9.:]/g, '').slice(0, 21)
		: v
				.replace(/\D/g, '')
				.slice(0, 9)
				.replace(/(\d{3})(?=\d)/g, '$1 ')
				.trim();

const ipRe = /^\d{1,3}(\.\d{1,3}){3}(:\d{1,5})?$/;

/** Whether `v` is a connectable target: a full IPv4 (optionally :port), or at least
 * 6 ID digits (a partial relay ID the relay can still resolve). */
export const canConnectTarget = (v: string): boolean =>
	isAddr(v) ? ipRe.test(v.trim()) : v.replace(/\D/g, '').length >= 6;
