/**
 * session/files.js — Remote file browser + transfer-progress queue (W4-files-js)
 *
 * DT-files:
 *   Single-column remote directory browser with breadcrumb + 44px tap rows,
 *   per-file download, OS document-picker upload, and a transfer-progress list
 *   with success/fail states. Remote-ONLY (modes:['remote']).
 *
 * Architecture (§1.1, §3 contract):
 *   - Registers itself with the overlay via registerCard() at import time.
 *   - Section 'tools', order 20 (after clipboard/chat section).
 *   - Listens to Tauri events: fs-entries, file-begin, file-progress, file-recv.
 *   - Calls Tauri commands: fs_list, fs_get, send_file.
 *   - OS file picker via <input type=file> (not tauri-plugin-dialog command,
 *     which is a Rust-side dependency; the JS side uses the DOM file input
 *     while the Cargo.toml dependency enables the Rust plugin for W4-rust-data).
 *
 * Touch-first design:
 *   - All rows ≥ 44px (--touch-min) — large tap targets for thumb use.
 *   - Breadcrumb is a horizontally scrollable strip.
 *   - Download button shown on each file row (right side, 44×44 minimum).
 *   - Transfer-progress queue scrollable at the top of the panel.
 *   - Indigo accent (--brand / var(--accent)); remote-only, never shown in game.
 *   - Inline styles injected once via _injectStyles(); no bundler needed.
 *
 * Events (Rust → JS, §2.3 contract):
 *   fs-entries   { slot, path, entries:[{name,dir,size}] }
 *   file-begin   { slot, id, name, size, chunks }
 *   file-progress{ slot, id, received, total }
 *   file-recv    { slot, id, name, savedPath }
 *
 * Commands (JS → Rust, §2.3 contract):
 *   fs_list      { slot, path }        → () (reply via fs-entries)
 *   fs_get       { slot, path }        → () (reply via file-* events)
 *   send_file    { slot, name, bytes } → { id }  (base64 bytes, CHUNK=2048 handled in Rust)
 */

import { invoke, listen } from '../tauri.js';
import { t }              from '../i18n.js';
import { registerCard }   from './overlay.js';

// ── Helpers ──────────────────────────────────────────────────────────────────

/** Format a file-size in bytes to a human-readable string. */
function _fmtSize(bytes) {
	if (bytes == null || bytes < 0) return '';
	if (bytes === 0) return '0 B';
	const units = ['B', 'KB', 'MB', 'GB'];
	let i = 0;
	let v = bytes;
	while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
	return (i === 0 ? v : v.toFixed(1)) + ' ' + units[i];
}

/** Clamp a value to [0, 1] for progress rendering. */
function _pct(received, total) {
	if (!total || total <= 0) return 0;
	return Math.min(1, Math.max(0, received / total));
}

/** Return a file extension icon (emoji or simple ASCII). */
function _fileIcon(name, isDir) {
	if (isDir) return '📁';
	const ext = (name.split('.').pop() || '').toLowerCase();
	const icons = {
		jpg: '🖼', jpeg: '🖼', png: '🖼', gif: '🖼', webp: '🖼', bmp: '🖼', svg: '🖼',
		mp4: '🎬', mkv: '🎬', avi: '🎬', mov: '🎬', wmv: '🎬', webm: '🎬',
		mp3: '🎵', flac: '🎵', wav: '🎵', ogg: '🎵', aac: '🎵', m4a: '🎵',
		pdf: '📄', doc: '📝', docx: '📝', xls: '📊', xlsx: '📊', ppt: '📊',
		zip: '🗜', rar: '🗜', tar: '🗜', gz: '🗜', '7z': '🗜',
		txt: '📃', md: '📃', csv: '📃', json: '📃', xml: '📃', log: '📃',
		exe: '⚙', msi: '⚙', sh: '⚙', bat: '⚙', app: '⚙',
	};
	return icons[ext] || '📄';
}

// ── Transfer queue state ──────────────────────────────────────────────────────

/**
 * @typedef {{ id:number, name:string, size:number, received:number, total:number,
 *             state:'pending'|'done'|'fail', direction:'dl'|'ul' }} XferItem
 */

/** @type {Map<number, XferItem>} — transfer id → item */
const _xfers = new Map();

/** Auto-increment for upload IDs (which originate client-side, not from Rust). */
let _ulIdSeq = 0x8000_0000;

// ── Browser state ──────────────────────────────────────────────────────────────

/** @type {number} — current session slot */
let _slot = 0;

/** @type {string[]} — breadcrumb path stack (each entry = a path segment) */
let _crumbs = [];

/** @type {{ name:string, dir:boolean, size:number }[]|null} — null while loading */
let _entries = null;

/** @type {boolean} */
let _loading = false;

/** @type {string|null} — error message or null */
let _error   = null;

// ── DOM references ──────────────────────────────────────────────────────────────

/** @type {HTMLElement|null} — card root, set in mount() */
let _el = null;

/** @type {HTMLInputElement|null} — hidden file picker */
let _filePicker = null;

// ── Tauri event unlisteners ─────────────────────────────────────────────────────

/** @type {Function[]} */
const _unlisten = [];

// ── Path helpers ─────────────────────────────────────────────────────────────────

/** Build the current absolute remote path from the breadcrumb stack. */
function _currentPath() {
	if (_crumbs.length === 0) return '/';
	return '/' + _crumbs.join('/');
}

/** Push a segment onto the breadcrumb and navigate into it. */
function _pushDir(segment) {
	_crumbs.push(segment);
	_loadDir(_currentPath());
}

/** Pop one level and navigate up. */
function _popDir() {
	if (_crumbs.length === 0) return;
	_crumbs.pop();
	_loadDir(_currentPath());
}

/** Navigate to a specific breadcrumb index (0 = root). */
function _navToCrumb(index) {
	_crumbs = _crumbs.slice(0, index);
	_loadDir(_currentPath());
}

// ── Data loading ─────────────────────────────────────────────────────────────────

/**
 * Request a directory listing from the host.
 * The host will reply with an 'fs-entries' event.
 * @param {string} path
 */
async function _loadDir(path) {
	_loading = true;
	_entries = null;
	_error   = null;
	_render();

	try {
		await invoke('fs_list', { slot: _slot, path });
	} catch (err) {
		_loading = false;
		_error   = String(err);
		_render();
	}
}

// ── Download ─────────────────────────────────────────────────────────────────────

/**
 * Request download of a single file from the host.
 * Progress arrives via file-begin / file-progress / file-recv events.
 * @param {string} name
 */
async function _downloadFile(name) {
	const remotePath = _currentPath() === '/'
		? '/' + name
		: _currentPath() + '/' + name;

	try {
		await invoke('fs_get', { slot: _slot, path: remotePath });
	} catch (err) {
		console.error('[files] fs_get error:', err);
	}
}

// ── Upload ────────────────────────────────────────────────────────────────────────

/** Trigger the OS document picker. */
function _pickAndUpload() {
	if (!_filePicker) {
		_filePicker = document.createElement('input');
		_filePicker.type     = 'file';
		_filePicker.multiple = false;
		_filePicker.style.display = 'none';
		document.body.appendChild(_filePicker);

		_filePicker.addEventListener('change', () => {
			const file = _filePicker.files?.[0];
			_filePicker.value = ''; // reset so the same file can be re-picked
			if (file) _sendFile(file);
		});
	}
	_filePicker.click();
}

/**
 * Read a File object and send it to the host via send_file.
 * @param {File} file
 */
async function _sendFile(file) {
	const MAX_BYTES = 50 * 1024 * 1024; // 50 MB guard
	if (file.size > MAX_BYTES) {
		_showToast(t('session.fileTooBig'), 'warn');
		return;
	}

	const id = _ulIdSeq++;
	/** @type {XferItem} */
	const item = {
		id,
		name:      file.name,
		size:      file.size,
		received:  0,
		total:     file.size,
		state:     'pending',
		direction: 'ul',
	};
	_xfers.set(id, item);
	_renderXferQueue();

	try {
		const buf    = await file.arrayBuffer();
		const bytes  = _arrayBufferToBase64(buf);

		await invoke('send_file', { slot: _slot, name: file.name, bytes });

		item.received = file.size;
		item.state    = 'done';
		_showToast(t('session.fileSent', { name: file.name }), 'ok');
	} catch (err) {
		item.state = 'fail';
		_showToast(t('session.fileError', { name: file.name }), 'danger');
		console.error('[files] send_file error:', err);
	}

	_renderXferQueue();

	// Auto-dismiss completed/failed entries after 8 s
	setTimeout(() => {
		if (_xfers.get(id)?.state !== 'pending') {
			_xfers.delete(id);
			_renderXferQueue();
		}
	}, 8000);
}

/** Convert ArrayBuffer → base64 string (chunked to avoid stack overflow). */
function _arrayBufferToBase64(buf) {
	const bytes = new Uint8Array(buf);
	let binary  = '';
	const CHUNK = 8192;
	for (let i = 0; i < bytes.length; i += CHUNK) {
		binary += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
	}
	return btoa(binary);
}

// ── Toast notifications ──────────────────────────────────────────────────────────

/** @param {string} msg  @param {'ok'|'warn'|'danger'} [kind] */
function _showToast(msg, kind = 'ok') {
	// Re-use session-level flash if available; otherwise a simple local one.
	const toast = document.createElement('div');
	toast.className = 'files-toast files-toast--' + kind;
	toast.textContent = msg;
	toast.setAttribute('role', 'status');
	toast.setAttribute('aria-live', 'polite');

	const anchor = _el?.closest('.overlay-card') || document.body;
	anchor.appendChild(toast);

	// Fade in
	requestAnimationFrame(() => toast.classList.add('visible'));

	// Dismiss after 3.5s
	setTimeout(() => {
		toast.classList.remove('visible');
		setTimeout(() => toast.remove(), 350);
	}, 3500);
}

// ── Tauri event wiring ──────────────────────────────────────────────────────────

async function _wireEvents() {
	// Clean up any previous listeners
	_unlisten.forEach((fn) => { try { fn(); } catch (_) {} });
	_unlisten.length = 0;

	_unlisten.push(
		await listen('fs-entries', ({ slot, path, entries }) => {
			if (slot !== _slot) return;
			_loading = false;
			_entries = entries || [];
			_error   = null;
			_render();
		}),

		await listen('file-begin', ({ slot, id, name, size }) => {
			if (slot !== _slot) return;
			/** @type {XferItem} */
			const item = {
				id,
				name,
				size,
				received: 0,
				total:    size,
				state:    'pending',
				direction: 'dl',
			};
			_xfers.set(id, item);
			_renderXferQueue();
		}),

		await listen('file-progress', ({ slot, id, received, total }) => {
			if (slot !== _slot) return;
			const item = _xfers.get(id);
			if (item) {
				item.received = received;
				item.total    = total;
				_renderXferQueue();
			}
		}),

		await listen('file-recv', ({ slot, id, name, savedPath }) => {
			if (slot !== _slot) return;
			const item = _xfers.get(id);
			if (item) {
				item.received = item.total;
				item.state    = 'done';
				_renderXferQueue();
			}
			_showToast(t('files.downloaded', { name }), 'ok');

			// Auto-dismiss the completed item after 8 s
			setTimeout(() => {
				if (_xfers.get(id)?.state !== 'pending') {
					_xfers.delete(id);
					_renderXferQueue();
				}
			}, 8000);
		}),
	);
}

// ── Render ───────────────────────────────────────────────────────────────────────

/**
 * Full re-render of the card content.
 * Called after any state change.
 */
function _render() {
	if (!_el) return;
	_el.innerHTML = '';

	// Card header row: title + upload button
	const header = document.createElement('div');
	header.className = 'files-header';
	header.innerHTML = `
		<span class="overlay-card-label files-title">${t('session.filesPanel')}</span>
		<button class="files-upload-btn icon-btn" type="button"
		        aria-label="${t('files.upload')}"
		        title="${t('files.upload')}">
			<svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
				<path d="M12 3v12M8 7l4-4 4 4" stroke="currentColor" stroke-width="2.2"
				      stroke-linecap="round" stroke-linejoin="round"/>
				<path d="M5 20h14" stroke="currentColor" stroke-width="2"
				      stroke-linecap="round"/>
			</svg>
		</button>
	`;
	header.querySelector('.files-upload-btn').addEventListener('click', () => _pickAndUpload());
	_el.appendChild(header);

	// Transfer-progress queue (always visible when non-empty)
	_renderXferQueue();

	// Breadcrumb navigation strip
	_renderBreadcrumb();

	// Directory content
	if (_loading) {
		const loader = document.createElement('div');
		loader.className   = 'files-state';
		loader.textContent = t('files.loading');
		_el.appendChild(loader);
		return;
	}

	if (_error) {
		const errEl = document.createElement('div');
		errEl.className = 'files-state files-state--error';
		errEl.innerHTML =
			`<span>${_error}</span>
			 <button class="btn btn-ghost files-retry" type="button">${t('m.retry')}</button>`;
		errEl.querySelector('.files-retry').addEventListener('click', () =>
			_loadDir(_currentPath())
		);
		_el.appendChild(errEl);
		return;
	}

	if (_entries === null) {
		// Not yet loaded — show the root-navigate prompt
		const prompt = document.createElement('button');
		prompt.className   = 'btn btn-ghost files-root-btn';
		prompt.textContent = t('files.home');
		prompt.type        = 'button';
		prompt.addEventListener('click', () => _loadDir('/'));
		_el.appendChild(prompt);
		return;
	}

	if (_entries.length === 0) {
		const empty = document.createElement('div');
		empty.className   = 'files-state';
		empty.textContent = t('files.empty');
		_el.appendChild(empty);
		return;
	}

	// Entry list
	const list = document.createElement('ul');
	list.className   = 'files-list';
	list.setAttribute('role', 'list');

	_entries.forEach((entry) => {
		const li = document.createElement('li');
		li.className = 'files-row';
		li.setAttribute('role', 'listitem');

		const icon    = _fileIcon(entry.name, entry.dir);
		const sizeStr = entry.dir ? '' : _fmtSize(entry.size);

		li.innerHTML = `
			<span class="files-row-icon" aria-hidden="true">${icon}</span>
			<span class="files-row-info">
				<span class="files-row-name">${_escapeHtml(entry.name)}</span>
				${sizeStr ? `<span class="files-row-size">${sizeStr}</span>` : ''}
			</span>
			${entry.dir
				? `<svg class="files-row-chevron" width="16" height="16" viewBox="0 0 24 24"
				       fill="none" aria-hidden="true">
						<path d="M9 6l6 6-6 6" stroke="currentColor" stroke-width="2.2"
						      stroke-linecap="round" stroke-linejoin="round"/>
					</svg>`
				: `<button class="files-dl-btn icon-btn" type="button"
				           aria-label="${t('files.download')} ${_escapeHtml(entry.name)}"
				           data-name="${_escapeHtml(entry.name)}">
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
							<path d="M12 3v12M8 11l4 4 4-4" stroke="currentColor" stroke-width="2.2"
							      stroke-linecap="round" stroke-linejoin="round"/>
							<path d="M5 20h14" stroke="currentColor" stroke-width="2"
							      stroke-linecap="round"/>
						</svg>
					</button>`
			}
		`;

		if (entry.dir) {
			li.setAttribute('role', 'button');
			li.tabIndex = 0;
			li.style.cursor = 'pointer';
			li.addEventListener('click', () => _pushDir(entry.name));
			li.addEventListener('keydown', (e) => {
				if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); _pushDir(entry.name); }
			});
		} else {
			const dlBtn = li.querySelector('.files-dl-btn');
			dlBtn?.addEventListener('click', (e) => {
				e.stopPropagation();
				_downloadFile(entry.name);
			});
		}

		list.appendChild(li);
	});

	_el.appendChild(list);
}

/** Re-render just the transfer-progress queue section. */
function _renderXferQueue() {
	if (!_el) return;

	// Remove existing queue element
	_el.querySelector('.files-xfer-queue')?.remove();

	if (_xfers.size === 0) return;

	const queue = document.createElement('div');
	queue.className = 'files-xfer-queue';

	_xfers.forEach((item) => {
		const pct    = Math.round(_pct(item.received, item.total) * 100);
		const label  = item.direction === 'dl' ? '↓' : '↑';
		const stateClass =
			item.state === 'done'   ? 'xfer-done'
			: item.state === 'fail' ? 'xfer-fail'
			: 'xfer-pending';

		const row = document.createElement('div');
		row.className = 'files-xfer-row ' + stateClass;

		row.innerHTML = `
			<span class="xfer-dir" aria-hidden="true">${label}</span>
			<span class="xfer-name" title="${_escapeHtml(item.name)}">${_escapeHtml(item.name)}</span>
			<span class="xfer-pct">${item.state === 'pending' ? pct + '%' : item.state === 'done' ? '✓' : '✗'}</span>
			${item.state === 'pending' ? `<div class="xfer-bar"><div class="xfer-fill" style="width:${pct}%"></div></div>` : ''}
		`;

		queue.appendChild(row);
	});

	// Insert after the header, before the breadcrumb
	const breadcrumb = _el.querySelector('.files-breadcrumb');
	if (breadcrumb) {
		_el.insertBefore(queue, breadcrumb);
	} else {
		const header = _el.querySelector('.files-header');
		if (header) {
			header.after(queue);
		} else {
			_el.prepend(queue);
		}
	}
}

/** Render the breadcrumb navigation strip. */
function _renderBreadcrumb() {
	if (!_el) return;

	_el.querySelector('.files-breadcrumb')?.remove();

	const strip = document.createElement('nav');
	strip.className = 'files-breadcrumb';
	strip.setAttribute('aria-label', 'Dizin yolu');

	// Root segment
	const rootBtn = document.createElement('button');
	rootBtn.type = 'button';
	rootBtn.className = 'files-crumb' + (_crumbs.length === 0 ? ' files-crumb--active' : '');
	rootBtn.textContent = '/';
	rootBtn.setAttribute('aria-current', _crumbs.length === 0 ? 'page' : undefined);
	rootBtn.addEventListener('click', () => _navToCrumb(0));
	strip.appendChild(rootBtn);

	_crumbs.forEach((seg, idx) => {
		const sep = document.createElement('span');
		sep.className   = 'files-crumb-sep';
		sep.textContent = '›';
		sep.setAttribute('aria-hidden', 'true');
		strip.appendChild(sep);

		const btn = document.createElement('button');
		btn.type = 'button';
		const isLast = idx === _crumbs.length - 1;
		btn.className  = 'files-crumb' + (isLast ? ' files-crumb--active' : '');
		btn.textContent = seg;
		btn.setAttribute('aria-current', isLast ? 'page' : undefined);
		btn.addEventListener('click', () => _navToCrumb(idx + 1));
		strip.appendChild(btn);
	});

	// "Up" button (hidden at root)
	if (_crumbs.length > 0) {
		const upBtn = document.createElement('button');
		upBtn.type = 'button';
		upBtn.className = 'files-up-btn icon-btn';
		upBtn.setAttribute('aria-label', t('files.up'));
		upBtn.title = t('files.up');
		upBtn.innerHTML = `
			<svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
				<path d="M6 15l6-6 6 6" stroke="currentColor" stroke-width="2.2"
				      stroke-linecap="round" stroke-linejoin="round"/>
			</svg>
		`;
		upBtn.addEventListener('click', () => _popDir());
		strip.appendChild(upBtn);
	}

	// Insert after xfer queue (if any) or after header
	const xferQueue = _el.querySelector('.files-xfer-queue');
	const header    = _el.querySelector('.files-header');

	if (xferQueue) {
		xferQueue.after(strip);
	} else if (header) {
		header.after(strip);
	} else {
		_el.prepend(strip);
	}
}

// ── XSS guard ────────────────────────────────────────────────────────────────────

const _esc = document.createElement('div');
/** @param {string} s @returns {string} */
function _escapeHtml(s) {
	_esc.textContent = s;
	return _esc.innerHTML;
}

// ── Styles ───────────────────────────────────────────────────────────────────────

function _injectStyles() {
	if (document.getElementById('files-module-styles')) return;
	const style = document.createElement('style');
	style.id = 'files-module-styles';
	style.textContent = `
/* ============================================================
   files.js — inline styles (dark-surface, inside overlay card)
   Brand = var(--brand) which swaps indigo→cyan in game mode,
   but this card is remote-only so it will always be indigo.
   ============================================================ */

/* ---- Card header ---- */
.files-header {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 8px;
	margin-bottom: 6px;
	min-height: var(--touch-min, 44px);
}

.files-title {
	flex: 1;
	/* overlay-card-label provides typography */
}

/* Upload button */
.files-upload-btn {
	width: 40px;
	height: 40px;
	min-width: 40px;
	min-height: 40px;
	display: flex;
	align-items: center;
	justify-content: center;
	border-radius: var(--r-sm, 9px);
	background: oklch(1 0 0 / 0.08);
	color: var(--brand, oklch(0.555 0.205 272));
	border: 1px solid oklch(1 0 0 / 0.12);
	transition: background 0.15s, transform 0.12s;
	-webkit-tap-highlight-color: transparent;
	cursor: pointer;
}
.files-upload-btn:active {
	background: oklch(1 0 0 / 0.18);
	transform: scale(0.93);
}

/* ---- Transfer queue ---- */
.files-xfer-queue {
	display: flex;
	flex-direction: column;
	gap: 5px;
	margin: 0 0 8px;
	padding: 8px 0 0;
	border-top: 1px solid oklch(1 0 0 / 0.08);
}

.files-xfer-row {
	display: grid;
	grid-template-columns: 18px 1fr 30px;
	grid-template-rows: auto auto;
	align-items: center;
	gap: 0 8px;
	padding: 4px 0;
	position: relative;
}

.xfer-dir {
	font-size: 12px;
	font-weight: 700;
	color: var(--brand, oklch(0.555 0.205 272));
	text-align: center;
	grid-row: 1;
}
.xfer-name {
	font-size: 12px;
	color: oklch(0.9 0 0);
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
	grid-row: 1;
}
.xfer-pct {
	font-family: var(--font-mono, 'JetBrains Mono', monospace);
	font-size: 11px;
	font-weight: 600;
	text-align: right;
	color: oklch(0.78 0 0);
	grid-row: 1;
}
.xfer-done .xfer-pct { color: oklch(0.63 0.15 158); }
.xfer-fail .xfer-pct { color: oklch(0.575 0.205 25); }

/* Progress bar spans full width on row 2 */
.xfer-bar {
	grid-column: 1 / -1;
	grid-row: 2;
	height: 3px;
	background: oklch(1 0 0 / 0.12);
	border-radius: 2px;
	overflow: hidden;
	margin-top: 4px;
}
.xfer-fill {
	height: 100%;
	background: var(--brand, oklch(0.555 0.205 272));
	border-radius: 2px;
	transition: width 0.2s linear;
}

/* ---- Breadcrumb strip ---- */
.files-breadcrumb {
	display: flex;
	align-items: center;
	gap: 0;
	overflow-x: auto;
	-webkit-overflow-scrolling: touch;
	scrollbar-width: none;
	padding: 6px 0 8px;
	border-bottom: 1px solid oklch(1 0 0 / 0.08);
	margin-bottom: 4px;
	min-height: var(--touch-min, 44px);
	/* Scroll to the end so active segment is visible */
	scroll-snap-type: x proximity;
}
.files-breadcrumb::-webkit-scrollbar { display: none; }

.files-crumb {
	font-family: var(--font-mono, 'JetBrains Mono', monospace);
	font-size: 12px;
	font-weight: 500;
	color: oklch(0.62 0 0);
	background: none;
	border: none;
	padding: 8px 5px;
	cursor: pointer;
	white-space: nowrap;
	border-radius: var(--r-xs, 6px);
	min-height: var(--touch-min, 44px);
	display: flex;
	align-items: center;
	-webkit-tap-highlight-color: transparent;
	transition: color 0.15s, background 0.15s;
	scroll-snap-align: end;
}
.files-crumb:active {
	background: oklch(1 0 0 / 0.08);
}
.files-crumb--active {
	color: var(--brand, oklch(0.555 0.205 272));
	font-weight: 700;
	cursor: default;
}
.files-crumb-sep {
	font-size: 13px;
	color: oklch(0.45 0 0);
	padding: 0 1px;
	flex-shrink: 0;
	user-select: none;
}

/* "Up" button in breadcrumb */
.files-up-btn {
	margin-left: auto;
	flex-shrink: 0;
	width: 36px;
	height: 36px;
	display: flex;
	align-items: center;
	justify-content: center;
	border-radius: var(--r-sm, 9px);
	background: oklch(1 0 0 / 0.07);
	color: oklch(0.72 0 0);
	border: 1px solid oklch(1 0 0 / 0.1);
	cursor: pointer;
	-webkit-tap-highlight-color: transparent;
	transition: background 0.15s;
}
.files-up-btn:active {
	background: oklch(1 0 0 / 0.16);
}

/* ---- State messages (loading / empty / error) ---- */
.files-state {
	font-size: 13px;
	color: oklch(0.62 0 0);
	text-align: center;
	padding: 20px 8px;
}
.files-state--error {
	color: oklch(0.68 0.12 25);
	display: flex;
	flex-direction: column;
	align-items: center;
	gap: 10px;
}
.files-retry {
	font-size: 13px;
	min-height: var(--touch-min, 44px);
	padding: 8px 18px;
}

/* Root navigate button */
.files-root-btn {
	width: 100%;
	min-height: var(--touch-min, 44px);
	font-size: 14px;
	font-weight: 600;
	color: var(--brand, oklch(0.555 0.205 272));
	border: 1px solid oklch(1 0 0 / 0.15);
	border-radius: var(--r, 13px);
	background: oklch(1 0 0 / 0.05);
	-webkit-tap-highlight-color: transparent;
	transition: background 0.15s;
}
.files-root-btn:active {
	background: oklch(1 0 0 / 0.12);
}

/* ---- Entry list ---- */
.files-list {
	list-style: none;
	margin: 0;
	padding: 0;
	display: flex;
	flex-direction: column;
	gap: 2px;
}

.files-row {
	display: flex;
	align-items: center;
	gap: 10px;
	min-height: var(--touch-min, 44px);
	padding: 6px 4px 6px 6px;
	border-radius: var(--r-sm, 9px);
	transition: background 0.14s;
	-webkit-tap-highlight-color: transparent;
}
.files-row:active,
.files-row:focus-visible {
	background: oklch(1 0 0 / 0.07);
	outline: none;
}

.files-row-icon {
	font-size: 20px;
	flex-shrink: 0;
	line-height: 1;
	width: 28px;
	text-align: center;
	/* prevent ligature merging */
	font-variant-emoji: emoji;
}

.files-row-info {
	flex: 1;
	min-width: 0;
	display: flex;
	flex-direction: column;
	gap: 1px;
}

.files-row-name {
	font-size: 14px;
	font-weight: 500;
	color: oklch(0.92 0 0);
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
}

.files-row-size {
	font-family: var(--font-mono, 'JetBrains Mono', monospace);
	font-size: 11px;
	color: oklch(0.58 0 0);
}

.files-row-chevron {
	flex-shrink: 0;
	color: oklch(0.5 0 0);
	margin-right: 2px;
}

/* Download button */
.files-dl-btn {
	flex-shrink: 0;
	width: 40px;
	height: 40px;
	min-width: 40px;
	display: flex;
	align-items: center;
	justify-content: center;
	border-radius: var(--r-sm, 9px);
	background: oklch(1 0 0 / 0.06);
	color: var(--brand, oklch(0.555 0.205 272));
	border: 1px solid oklch(1 0 0 / 0.1);
	cursor: pointer;
	-webkit-tap-highlight-color: transparent;
	transition: background 0.15s, transform 0.12s;
}
.files-dl-btn:active {
	background: oklch(1 0 0 / 0.16);
	transform: scale(0.9);
}

/* ---- Toast notifications ---- */
.files-toast {
	position: absolute;
	bottom: 60px;
	left: 12px;
	right: 12px;
	z-index: 20;
	padding: 10px 14px;
	border-radius: var(--r, 13px);
	font-size: 13px;
	font-weight: 500;
	color: oklch(0.97 0 0);
	background: oklch(0.25 0.02 268 / 0.92);
	box-shadow: 0 4px 20px oklch(0 0 0 / 0.35);
	opacity: 0;
	transform: translateY(8px);
	transition: opacity 0.25s var(--ease, cubic-bezier(0.16,1,0.3,1)),
	            transform 0.25s var(--ease, cubic-bezier(0.16,1,0.3,1));
	pointer-events: none;
}
.files-toast.visible {
	opacity: 1;
	transform: translateY(0);
}
.files-toast--ok     { background: oklch(0.32 0.06 158 / 0.92); }
.files-toast--warn   { background: oklch(0.38 0.1 75  / 0.92); }
.files-toast--danger { background: oklch(0.35 0.08 25  / 0.92); }
`;
	document.head.appendChild(style);
}

// ── Card mount ───────────────────────────────────────────────────────────────────

/**
 * Called by the overlay when it inserts this card's DOM element.
 * @param {HTMLElement} el
 */
function _mount(el) {
	_el = el;
	_el.classList.add('files-card');

	// Determine the active slot from the bus (session-started stores it on body or bus)
	const busSlot = window.__pulsarBus?._activeSlot ?? 0;
	_slot = typeof busSlot === 'number' ? busSlot : 0;

	// Wire events
	_wireEvents().catch((e) => console.warn('[files] event wire error:', e));

	// Initial render (shows "home" button to trigger the first load)
	_render();

	// Listen for slot changes from the bus
	window.__pulsarBus?.on('session-started', ({ slot }) => {
		if (typeof slot === 'number') {
			_slot = slot;
			// Reset browser state for the new session
			_crumbs  = [];
			_entries = null;
			_loading = false;
			_error   = null;
			_render();
		}
	});

	window.__pulsarBus?.on('session-ended', () => {
		// Tear down listeners and reset state
		_unlisten.forEach((fn) => { try { fn(); } catch (_) {} });
		_unlisten.length = 0;
		_xfers.clear();
		_crumbs  = [];
		_entries = null;
		_loading = false;
		_error   = null;
		if (_el) _render();
	});
}

// ── Self-registration ─────────────────────────────────────────────────────────────

_injectStyles();

registerCard({
	id:      'files',
	modes:   ['remote'],          // remote-only per §1.2 and DT-files
	section: 'tools',
	order:   20,
	mount:   _mount,
});
