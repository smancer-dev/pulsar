import { describe, it, expect } from 'vitest';
import { isAddr, fmtTarget, canConnectTarget } from './connectTarget';

describe('connectTarget', () => {
	it('isAddr detects IP/IP:port vs relay id', () => {
		expect(isAddr('192.168.1.42')).toBe(true);
		expect(isAddr('192.168.1.42:9000')).toBe(true);
		expect(isAddr('641 724 395')).toBe(false);
		expect(isAddr('641724395')).toBe(false);
	});

	it('fmtTarget groups a relay id in threes and caps at 9 digits', () => {
		expect(fmtTarget('641724395')).toBe('641 724 395');
		expect(fmtTarget('64172439599999')).toBe('641 724 395');
		expect(fmtTarget('abc641')).toBe('641'); // non-digits stripped
	});

	it('fmtTarget keeps address chars (digits, dots, colons) and caps length', () => {
		expect(fmtTarget('192.168.1.42:9000')).toBe('192.168.1.42:9000');
		expect(fmtTarget('192.168.1.42abc')).toBe('192.168.1.42'); // letters stripped → still an addr
	});

	it('canConnectTarget: full IPv4 (optional port), or ≥6 id digits', () => {
		expect(canConnectTarget('192.168.1.42')).toBe(true);
		expect(canConnectTarget('192.168.1.42:9000')).toBe(true);
		expect(canConnectTarget('192.168.1')).toBe(false); // incomplete IPv4
		expect(canConnectTarget('641 724')).toBe(true); // 6 digits
		expect(canConnectTarget('641 7')).toBe(false); // 5 digits
	});
});
