// @vitest-environment node
// (No DOM needed — applyCtrlSwap is pure logic; avoids jsdom ESM issues.)
import { describe, it, expect } from 'vitest';
import { applyCtrlSwap } from './settings.svelte';

describe('applyCtrlSwap', () => {
	it('swaps two existing entries', () => {
		const order = ['aaa', 'bbb', 'ccc'];
		const ok = applyCtrlSwap(order, () => '', 0, 1);
		expect(ok).toBe(true);
		expect(order).toEqual(['bbb', 'aaa', 'ccc']);
	});

	it('returns false for same index', () => {
		const order = ['aaa', 'bbb'];
		const ok = applyCtrlSwap(order, () => '', 1, 1);
		expect(ok).toBe(false);
		expect(order).toEqual(['aaa', 'bbb']); // unchanged
	});

	it('seeds missing slots before swapping', () => {
		// order has only slot 0; slots 1 and 2 need to be seeded
		const order = ['aaa'];
		const pads = ['bbb', 'ccc'];
		let seedIdx = 0;
		const ok = applyCtrlSwap(order, () => pads[seedIdx++] ?? '', 0, 2);
		expect(ok).toBe(true);
		// After seeding: ['aaa', 'bbb', 'ccc'], then swap(0,2) → ['ccc', 'bbb', 'aaa']
		expect(order).toEqual(['ccc', 'bbb', 'aaa']);
	});

	it('handles an empty order array — seeds both slots then swaps', () => {
		const pads = ['p0', 'p1'];
		let idx = 0;
		const order: string[] = [];
		const ok = applyCtrlSwap(order, () => pads[idx++] ?? '', 0, 1);
		expect(ok).toBe(true);
		// seeded ['p0', 'p1'], swap(0,1) → ['p1', 'p0']
		expect(order).toEqual(['p1', 'p0']);
	});

	it('returns false for negative indices', () => {
		const order = ['aaa', 'bbb'];
		expect(applyCtrlSwap(order, () => '', -1, 0)).toBe(false);
	});
});
