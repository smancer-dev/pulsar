<script lang="ts">
	// Line-icon set ported from the design's shell.jsx. Paths are static, so
	// rendering them via {@html} is safe.
	type Props = { name: string; size?: number; stroke?: number; class?: string };
	let { name, size = 20, stroke = 2, class: cls = '' }: Props = $props();

	const paths: Record<string, string> = {
		connect: '<path d="M12 3v12"/><path d="M8 7l4-4 4 4"/><path d="M4 14v5a2 2 0 002 2h12a2 2 0 002-2v-5"/>',
		home: '<path d="M3 11l9-7 9 7"/><path d="M5 10v9a1 1 0 001 1h12a1 1 0 001-1v-9"/><path d="M9.5 20v-6h5v6"/>',
		devices: '<rect x="3" y="4" width="13" height="9" rx="1.5"/><rect x="13" y="9" width="8" height="11" rx="1.5"/><path d="M6 17h5M8 13v4"/>',
		gaming: '<rect x="2" y="6" width="20" height="12" rx="4"/><path d="M7 10v4M5 12h4"/><circle cx="15.5" cy="11" r="1"/><circle cx="18" cy="14" r="1"/>',
		settings: '<circle cx="12" cy="12" r="3"/><path d="M19 12a7 7 0 00-.1-1.3l2-1.5-2-3.4-2.3 1a7 7 0 00-2.2-1.3L14 2h-4l-.4 2.2a7 7 0 00-2.2 1.3l-2.3-1-2 3.4 2 1.5A7 7 0 005 12c0 .4 0 .9.1 1.3l-2 1.5 2 3.4 2.3-1a7 7 0 002.2 1.3L10 22h4l.4-2.2a7 7 0 002.2-1.3l2.3 1 2-3.4-2-1.5c.1-.4.1-.9.1-1.3z"/>',
		monitor: '<rect x="3" y="4" width="18" height="13" rx="2"/><path d="M8 21h8M12 17v4"/>',
		search: '<circle cx="11" cy="11" r="7"/><path d="M21 21l-4-4"/>',
		plus: '<path d="M12 5v14M5 12h14"/>',
		refresh: '<path d="M3 12a9 9 0 0115.5-6.3L21 8M21 4v4h-4"/><path d="M21 12a9 9 0 01-15.5 6.3L3 16M3 20v-4h4"/>',
		expand: '<path d="M8 3H5a2 2 0 00-2 2v3M16 3h3a2 2 0 012 2v3M8 21H5a2 2 0 01-2-2v-3M16 21h3a2 2 0 002-2v-3"/>',
		keyboard: '<rect x="2" y="6" width="20" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>',
		clipboard: '<rect x="8" y="3" width="8" height="4" rx="1"/><path d="M8 5H6a2 2 0 00-2 2v12a2 2 0 002 2h12a2 2 0 002-2V7a2 2 0 00-2-2h-2"/>',
		power: '<path d="M12 4v8M7.5 7a7 7 0 109 0"/>',
		file: '<path d="M14 3v4a1 1 0 001 1h4"/><path d="M5 3h9l5 5v11a2 2 0 01-2 2H5a2 2 0 01-2-2V5a2 2 0 012-2z"/>',
		folder: '<path d="M3 7a2 2 0 012-2h4l2 2h9a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z"/>',
		download: '<path d="M12 3v12M7 10l5 5 5-5"/><path d="M4 20h16"/>',
		upload: '<path d="M12 15V3M7 8l5-5 5 5"/><path d="M4 20h16"/>',
		chat: '<path d="M21 15a2 2 0 01-2 2H7l-4 4V5a2 2 0 012-2h14a2 2 0 012 2z"/>',
		mic: '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0014 0M12 18v3"/>',
		shield: '<path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z"/>',
		bolt: '<path d="M13 2L4 14h7l-1 8 9-12h-7z"/>',
		plug: '<path d="M9 2v6M15 2v6M7 8h10v3a5 5 0 01-10 0zM12 16v6"/>',
		star: '<path d="M12 3l2.6 5.3 5.9.8-4.3 4.1 1 5.8L12 16.3 6.8 19l1-5.8L3.5 9.1l5.9-.8z"/>',
		x: '<path d="M6 6l12 12M18 6L6 18"/>',
		check: '<path d="M4 12l5 5L20 6"/>',
		copy: '<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 012-2h10"/>',
		arrowRight: '<path d="M5 12h14M13 6l6 6-6 6"/>',
		wifi: '<path d="M5 12.5a10 10 0 0114 0M8.5 16a5 5 0 017 0M12 19.5h.01"/>',
		sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M2 12h2M20 12h2M5 5l1.5 1.5M17.5 17.5L19 19M19 5l-1.5 1.5M6.5 17.5L5 19"/>',
		speaker: '<path d="M11 5L6 9H3v6h3l5 4z"/><path d="M16 9a4 4 0 010 6M19 6a8 8 0 010 12"/>',
		globe: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18"/><path d="M12 3c2.5 2.6 3.8 5.7 3.8 9s-1.3 6.4-3.8 9c-2.5-2.6-3.8-5.7-3.8-9S9.5 5.6 12 3z"/>',
		menu: '<path d="M4 7h16M4 12h16M4 17h16"/>',
		edit: '<path d="M17 3a2.8 2.8 0 014 4L7.5 20.5 3 21l.5-4.5z"/>',
		grip: '<circle cx="9" cy="6" r="1"/><circle cx="15" cy="6" r="1"/><circle cx="9" cy="12" r="1"/><circle cx="15" cy="12" r="1"/><circle cx="9" cy="18" r="1"/><circle cx="15" cy="18" r="1"/>'
	};
</script>

<svg
	width={size}
	height={size}
	viewBox="0 0 24 24"
	fill="none"
	stroke="currentColor"
	stroke-width={stroke}
	stroke-linecap="round"
	stroke-linejoin="round"
	class={cls}
	aria-hidden="true">{@html paths[name] ?? ''}</svg
>
