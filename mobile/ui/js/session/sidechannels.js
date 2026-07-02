/**
 * sidechannels.js — Clipboard + chat side-channel UI (W4-sidechannels)
 *
 * DT-clipboard-chat:
 *   - Clipboard send/receive control: send button + inbound "Panoya kopyala" toast
 *   - Chat panel: chat icon with unread badge, bottom-sheet message list +
 *     visualViewport-aware composer over the native surface
 *
 * Remote-only — registered with modes: ['remote'].
 *
 * Tauri commands:
 *   send_clipboard  { slot, text: String }  → ()
 *   send_chat       { slot, text: String }  → ()
 *
 * Tauri events (listened here):
 *   clipboard-in    { slot, text }
 *   chat-msg        { slot, text }
 *
 * Design principles (DT-clipboard-chat, §4):
 *   - Large 44px+ tap targets on phone
 *   - visualViewport-aware chat composer: stays above the soft keyboard
 *   - Bottom-sheet chat panel over the transparent video surface
 *   - Indigo (--brand) accent for remote mode
 *   - Unread badge on the chat card header
 *   - My/them message bubble styling
 *   - Toast for inbound clipboard with "Panoya kopyala" action
 *   - Safe-area insets respected
 */

import { invoke, listen, clipboard } from '../tauri.js';
import { t } from '../i18n.js';

// ── State ────────────────────────────────────────────────────────────────────

/** Active session slot. */
let _slot = 0;

/**
 * Chat message log.
 * @type {Array<{from:'me'|'peer', text:string, ts:number}>}
 */
let _messages = [];

/** Count of unread messages (peer → me) while chat panel is closed. */
let _unread = 0;

/** Whether the chat panel is currently open. */
let _chatOpen = false;

/** Unlisten handles for Tauri events. */
let _unlistenClipboard = null;
let _unlistenChat      = null;

/** Toast timer id (auto-dismiss). */
let _toastTimer = null;

// ── DOM helpers ──────────────────────────────────────────────────────────────

const $ = (id) => document.getElementById(id);

// ── Clipboard card UI ────────────────────────────────────────────────────────

function _clipboardCardHTML() {
	return `
<div class="sc-card" id="sc-clip-card" role="region" aria-label="${t('session.clipboard')}">
  <div class="sc-card-header">
    <span class="sc-card-icon" aria-hidden="true">
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
        <rect x="9" y="2" width="6" height="4" rx="1" stroke="currentColor" stroke-width="1.75"/>
        <path d="M7 4H5a2 2 0 00-2 2v14a2 2 0 002 2h14a2 2 0 002-2V6a2 2 0 00-2-2h-2"
              stroke="currentColor" stroke-width="1.75" stroke-linecap="round"/>
        <path d="M9 12h6M9 16h4" stroke="currentColor" stroke-width="1.75" stroke-linecap="round"/>
      </svg>
    </span>
    <span class="sc-card-title">${t('session.clipboard')}</span>
  </div>
  <div class="sc-card-body">
    <button class="sc-primary-btn" id="sc-send-clip" type="button"
            aria-label="${t('session.clipboard')}">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <path d="M22 2L11 13" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
        <path d="M22 2L15 22L11 13L2 9L22 2Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
      <span>${t('session.clipboard')}</span>
    </button>
    <p class="sc-hint">${_t('m.sc.clipHint')}</p>
  </div>
</div>
`;
}

// ── Chat card UI ─────────────────────────────────────────────────────────────

function _chatCardHTML() {
	return `
<div class="sc-card" id="sc-chat-card" role="region" aria-label="${t('session.chat')}">
  <div class="sc-card-header">
    <span class="sc-card-icon" aria-hidden="true">
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
        <path d="M21 15a2 2 0 01-2 2H7l-4 4V5a2 2 0 012-2h14a2 2 0 012 2z"
              stroke="currentColor" stroke-width="1.75" stroke-linejoin="round"/>
      </svg>
    </span>
    <span class="sc-card-title">${t('session.chat')}</span>
    <span class="sc-unread-badge" id="sc-unread-badge"
          aria-live="polite" aria-label="${_t('m.sc.unreadAriaLabel', { n: _unread })}">
      ${_unread > 0 ? _unread : ''}
    </span>
  </div>
  <div class="sc-chat-preview" id="sc-chat-preview">
    ${_lastChatPreview()}
  </div>
  <button class="sc-primary-btn sc-open-chat-btn" id="sc-open-chat" type="button">
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path d="M21 15a2 2 0 01-2 2H7l-4 4V5a2 2 0 012-2h14a2 2 0 012 2z"
            stroke="currentColor" stroke-width="1.75" stroke-linejoin="round"/>
    </svg>
    <span>${_t('m.sc.openChat')}</span>
  </button>
</div>
`;
}

function _lastChatPreview() {
	if (_messages.length === 0) {
		return `<p class="sc-no-msg">${t('session.chatEmpty')}</p>`;
	}
	const last = _messages[_messages.length - 1];
	const label = last.from === 'me' ? t('session.chatYou') : t('session.chatPeer');
	return `<p class="sc-preview-text"><span class="sc-preview-from">${label}:</span> ${_escHtml(last.text)}</p>`;
}

// ── Mount (called by overlay.js when the card DOM element is ready) ───────────

function mount(container) {
	container.innerHTML = _clipboardCardHTML() + _chatCardHTML();
	_wireClipboard();
	_wireChatOpen();
	_updateUnreadBadge();
}

function _wireClipboard() {
	const btn = $('sc-send-clip');
	if (!btn) return;
	btn.addEventListener('click', async () => {
		btn.disabled = true;
		btn.classList.add('loading');
		try {
			// Read the system clipboard and send it to the remote
			let text = '';
			try {
				text = await navigator.clipboard.readText();
			} catch (_) {
				// Fallback: use empty string (the host side handles empty gracefully)
			}
			if (!text) {
				_showToast(_t('m.sc.clipboardEmpty'), 'warn');
				return;
			}
			await invoke('send_clipboard', { slot: _slot, text });
			_showToast(_t('m.sc.clipboardSent'), 'ok');
		} catch (err) {
			console.warn('[sidechannels] send_clipboard error:', err);
			_showToast(_t('m.sc.clipboardError'), 'err');
		} finally {
			btn.disabled = false;
			btn.classList.remove('loading');
		}
	});
}

function _wireChatOpen() {
	const btn = $('sc-open-chat');
	if (btn) btn.addEventListener('click', () => _openChatSheet());
}

// ── Chat bottom-sheet ─────────────────────────────────────────────────────────

/**
 * Open the full chat bottom-sheet.
 * The sheet is appended to <body> so it sits over the transparent video surface
 * and can respond to visualViewport resizes (soft keyboard).
 */
function _openChatSheet() {
	if (_chatOpen) return;
	_chatOpen  = true;
	_unread    = 0;
	_updateUnreadBadge();

	const sheet = document.createElement('div');
	sheet.id        = 'sc-chat-sheet';
	sheet.className = 'sc-sheet';
	sheet.setAttribute('role', 'dialog');
	sheet.setAttribute('aria-modal', 'true');
	sheet.setAttribute('aria-label', t('session.chat'));

	sheet.innerHTML = `
<div class="sc-sheet-backdrop" id="sc-sheet-backdrop" aria-hidden="true"></div>
<div class="sc-sheet-panel" id="sc-sheet-panel">
  <div class="sc-sheet-handle" aria-hidden="true"></div>
  <div class="sc-sheet-header">
    <span class="sc-sheet-title">${t('session.chat')}</span>
    <button class="sc-sheet-close icon-btn" id="sc-chat-close" type="button"
            aria-label="${t('m.close')}">
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <path d="M6 6l12 12M18 6L6 18"
              stroke="currentColor" stroke-width="2.2" stroke-linecap="round"/>
      </svg>
    </button>
  </div>
  <div class="sc-msg-list" id="sc-msg-list" aria-live="polite" aria-label="${t('session.chat')}">
    ${_renderMessages()}
  </div>
  <div class="sc-composer" id="sc-composer">
    <textarea
      id="sc-chat-input"
      class="sc-chat-input"
      placeholder="${t('session.chatPlaceholder')}"
      aria-label="${t('session.chatPlaceholder')}"
      rows="1"
      maxlength="2000"
    ></textarea>
    <button class="sc-send-btn" id="sc-send-msg" type="button"
            aria-label="${t('session.send')}" disabled>
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <path d="M22 2L11 13" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
        <path d="M22 2L15 22L11 13L2 9L22 2Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
    </button>
  </div>
</div>
`;

	document.body.appendChild(sheet);

	// Animate in
	requestAnimationFrame(() => {
		sheet.querySelector('.sc-sheet-panel')?.classList.add('open');
		sheet.querySelector('.sc-sheet-backdrop')?.classList.add('open');
	});

	// Wire events
	$('sc-chat-close')?.addEventListener('click', () => _closeChatSheet());
	$('sc-sheet-backdrop')?.addEventListener('click', () => _closeChatSheet());

	const input  = $('sc-chat-input');
	const sendBtn = $('sc-send-msg');

	// Auto-resize textarea
	if (input) {
		input.addEventListener('input', () => {
			input.style.height = 'auto';
			input.style.height = Math.min(input.scrollHeight, 120) + 'px';
			if (sendBtn) sendBtn.disabled = !input.value.trim();
		});
	}

	// Send on button click
	if (sendBtn) {
		sendBtn.addEventListener('click', () => _sendChat());
	}

	// Send on Enter (Shift+Enter = newline)
	if (input) {
		input.addEventListener('keydown', (e) => {
			if (e.key === 'Enter' && !e.shiftKey) {
				e.preventDefault();
				_sendChat();
			}
		});
	}

	// visualViewport: keep the composer above the soft keyboard
	_attachViewportListener();

	// Scroll to bottom
	_scrollMsgListToBottom();

	// Focus input
	setTimeout(() => input?.focus(), 120);
}

function _closeChatSheet() {
	if (!_chatOpen) return;
	_chatOpen = false;
	_detachViewportListener();

	const sheet = $('sc-chat-sheet');
	if (!sheet) return;

	const panel = sheet.querySelector('.sc-sheet-panel');
	const backdrop = sheet.querySelector('.sc-sheet-backdrop');
	if (panel) panel.classList.remove('open');
	if (backdrop) backdrop.classList.remove('open');

	// Remove after transition
	sheet.addEventListener('transitionend', () => {
		sheet.remove();
	}, { once: true });
	// Fallback removal
	setTimeout(() => { sheet.remove(); }, 350);
}

// ── visualViewport — composer stays above soft keyboard ────────────────────

let _vvHandler = null;

function _attachViewportListener() {
	if (!window.visualViewport) return;
	_vvHandler = _onViewportResize;
	window.visualViewport.addEventListener('resize', _vvHandler);
	window.visualViewport.addEventListener('scroll', _vvHandler);
	_onViewportResize();
}

function _detachViewportListener() {
	if (!window.visualViewport || !_vvHandler) return;
	window.visualViewport.removeEventListener('resize', _vvHandler);
	window.visualViewport.removeEventListener('scroll', _vvHandler);
	_vvHandler = null;
}

function _onViewportResize() {
	const vv = window.visualViewport;
	if (!vv) return;
	const panel = $('sc-sheet-panel');
	if (!panel) return;

	// Bottom of viewport (accounts for keyboard pushing the viewport up)
	const offsetBottom = window.innerHeight - (vv.offsetTop + vv.height);
	const safeBottom   = parseInt(
		getComputedStyle(document.documentElement)
			.getPropertyValue('--safe-bottom') || '0', 10
	) || 0;

	panel.style.bottom = Math.max(offsetBottom, safeBottom) + 'px';
}

// ── Chat: render + send ────────────────────────────────────────────────────

function _renderMessages() {
	if (_messages.length === 0) {
		return `<div class="sc-empty-msg">${t('session.chatEmpty')}</div>`;
	}
	return _messages.map((msg) => {
		const isMe = msg.from === 'me';
		const label = isMe ? t('session.chatYou') : t('session.chatPeer');
		const time  = _formatTime(msg.ts);
		return `
<div class="sc-bubble-row ${isMe ? 'me' : 'peer'}" role="listitem">
  <div class="sc-bubble ${isMe ? 'sc-bubble-me' : 'sc-bubble-peer'}">
    <span class="sc-bubble-sender">${_escHtml(label)}</span>
    <span class="sc-bubble-text">${_escHtml(msg.text)}</span>
    <span class="sc-bubble-time" aria-hidden="true">${time}</span>
  </div>
</div>`;
	}).join('');
}

async function _sendChat() {
	const input = $('sc-chat-input');
	const sendBtn = $('sc-send-msg');
	if (!input) return;
	const text = input.value.trim();
	if (!text) return;

	input.value = '';
	input.style.height = 'auto';
	if (sendBtn) sendBtn.disabled = true;

	// Optimistically append my message
	_messages.push({ from: 'me', text, ts: Date.now() });
	_refreshMsgList();
	_scrollMsgListToBottom();
	// Also refresh the card preview
	_refreshChatCardPreview();

	try {
		await invoke('send_chat', { slot: _slot, text });
	} catch (err) {
		console.warn('[sidechannels] send_chat error:', err);
	} finally {
		// The send button was disabled on send; re-sync it to the (now empty) input
		// so it doesn't get stuck disabled if the input event doesn't fire.
		if (sendBtn) sendBtn.disabled = !input.value.trim();
	}
}

function _refreshMsgList() {
	const list = $('sc-msg-list');
	if (list) list.innerHTML = _renderMessages();
}

function _scrollMsgListToBottom() {
	const list = $('sc-msg-list');
	if (list) list.scrollTop = list.scrollHeight;
}

function _refreshChatCardPreview() {
	const preview = $('sc-chat-preview');
	if (preview) preview.innerHTML = _lastChatPreview();
}

// ── Inbound event handlers ────────────────────────────────────────────────

/**
 * Handle an inbound clipboard-in event from the Rust side.
 * Shows a toast with a "Panoya kopyala" action button.
 */
function _onClipboardIn({ slot, text }) {
	if (slot !== _slot) return;
	_showClipboardToast(text);
}

/**
 * Handle an inbound chat-msg event from the Rust side.
 */
function _onChatMsg({ slot, text }) {
	if (slot !== _slot) return;
	_messages.push({ from: 'peer', text, ts: Date.now() });

	if (_chatOpen) {
		_refreshMsgList();
		_scrollMsgListToBottom();
	} else {
		_unread++;
		_updateUnreadBadge();
		_refreshChatCardPreview();
	}
}

// ── Clipboard toast ────────────────────────────────────────────────────────

/**
 * Show a prominent "Pano alındı" toast with a copy action.
 * @param {string} text — clipboard content from the remote
 */
function _showClipboardToast(text) {
	// Remove any existing clipboard toast
	$('sc-clip-toast')?.remove();

	const toast = document.createElement('div');
	toast.id        = 'sc-clip-toast';
	toast.className = 'sc-toast sc-toast-clip';
	toast.setAttribute('role', 'status');
	toast.setAttribute('aria-live', 'polite');

	const preview = text.length > 60 ? text.slice(0, 57) + '…' : text;
	toast.innerHTML = `
<div class="sc-toast-body">
  <svg class="sc-toast-icon" width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <rect x="9" y="2" width="6" height="4" rx="1" stroke="currentColor" stroke-width="1.75"/>
    <path d="M7 4H5a2 2 0 00-2 2v14a2 2 0 002 2h14a2 2 0 002-2V6a2 2 0 00-2-2h-2"
          stroke="currentColor" stroke-width="1.75" stroke-linecap="round"/>
  </svg>
  <span class="sc-toast-text">${_t('m.sc.clipReceived')}</span>
  <span class="sc-toast-preview">${_escHtml(preview)}</span>
</div>
<div class="sc-toast-actions">
  <button class="sc-toast-action" id="sc-clip-copy-btn" type="button">
    ${_t('m.sc.clipCopy')}
  </button>
  <button class="sc-toast-dismiss" id="sc-clip-dismiss-btn" type="button"
          aria-label="${t('m.close')}">
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path d="M6 6l12 12M18 6L6 18"
            stroke="currentColor" stroke-width="2.2" stroke-linecap="round"/>
    </svg>
  </button>
</div>
`;

	document.body.appendChild(toast);

	// Animate in
	requestAnimationFrame(() => toast.classList.add('visible'));

	// Wire copy button
	$('sc-clip-copy-btn')?.addEventListener('click', async () => {
		await clipboard(text);
		_dismissClipToast();
		_showToast(_t('m.sc.clipboardCopied'), 'ok');
	});

	// Wire dismiss
	$('sc-clip-dismiss-btn')?.addEventListener('click', () => _dismissClipToast());

	// Auto-dismiss after 6s
	if (_toastTimer) clearTimeout(_toastTimer);
	_toastTimer = setTimeout(() => _dismissClipToast(), 6000);
}

function _dismissClipToast() {
	if (_toastTimer) { clearTimeout(_toastTimer); _toastTimer = null; }
	const toast = $('sc-clip-toast');
	if (!toast) return;
	toast.classList.remove('visible');
	toast.addEventListener('transitionend', () => toast.remove(), { once: true });
	setTimeout(() => toast.remove(), 300);
}

// ── Simple status toast (send feedback) ──────────────────────────────────────

/**
 * Show a brief status toast (auto-dismisses in 2.5s).
 * @param {string} msg
 * @param {'ok'|'warn'|'err'} [kind]
 */
function _showToast(msg, kind = 'ok') {
	$('sc-status-toast')?.remove();
	const toast = document.createElement('div');
	toast.id        = 'sc-status-toast';
	toast.className = `sc-toast sc-toast-status sc-toast-${kind}`;
	toast.setAttribute('role', 'status');
	toast.setAttribute('aria-live', 'polite');
	toast.textContent = msg;
	document.body.appendChild(toast);
	requestAnimationFrame(() => toast.classList.add('visible'));
	setTimeout(() => {
		toast.classList.remove('visible');
		toast.addEventListener('transitionend', () => toast.remove(), { once: true });
		setTimeout(() => toast.remove(), 300);
	}, 2500);
}

// ── Unread badge ─────────────────────────────────────────────────────────────

function _updateUnreadBadge() {
	const badge = $('sc-unread-badge');
	if (!badge) return;
	if (_unread > 0) {
		badge.textContent = _unread > 99 ? '99+' : String(_unread);
		badge.style.display = 'inline-flex';
		badge.setAttribute('aria-label', _t('m.sc.unreadAriaLabel', { n: _unread }));
	} else {
		badge.textContent = '';
		badge.style.display = 'none';
	}
}

// ── Tauri event subscriptions ─────────────────────────────────────────────────

async function _subscribe() {
	try {
		_unlistenClipboard = await listen('clipboard-in', _onClipboardIn);
	} catch (e) {
		console.warn('[sidechannels] listen clipboard-in failed:', e);
	}
	try {
		_unlistenChat = await listen('chat-msg', _onChatMsg);
	} catch (e) {
		console.warn('[sidechannels] listen chat-msg failed:', e);
	}
}

function _unsubscribe() {
	if (typeof _unlistenClipboard === 'function') {
		_unlistenClipboard();
		_unlistenClipboard = null;
	}
	if (typeof _unlistenChat === 'function') {
		_unlistenChat();
		_unlistenChat = null;
	}
}

// ── Bus wiring ────────────────────────────────────────────────────────────────

function _wireBus() {
	const tryBus = () => {
		const bus = window.__pulsarBus;
		if (!bus) return false;

		bus.on('session-started', ({ slot }) => {
			_slot     = slot ?? 0;
			_messages = [];
			_unread   = 0;
			_chatOpen = false;
			_dismissClipToast();
			_subscribe();
		});

		bus.on('session-ended', () => {
			_closeChatSheet();
			_dismissClipToast();
			_unsubscribe();
			_messages = [];
			_unread   = 0;
		});

		return true;
	};

	if (!tryBus()) {
		if (document.readyState === 'loading') {
			document.addEventListener('DOMContentLoaded', () => tryBus());
		} else {
			setTimeout(() => tryBus(), 0);
		}
	}
}

// ── Overlay card registration ─────────────────────────────────────────────────

function _register() {
	const tryReg = () => {
		const overlay = window.__pulsarOverlay;
		if (!overlay || typeof overlay.registerCard !== 'function') return false;

		overlay.registerCard({
			id:      'sidechannels',
			modes:   ['remote'],      // remote-only — strictly hidden in game mode
			section: 'tools',
			order:   10,
			label:   () => _t('m.sc.cardLabel'),
			mount,
		});
		return true;
	};

	if (!tryReg()) {
		if (document.readyState === 'loading') {
			document.addEventListener('DOMContentLoaded', () => tryReg());
		} else {
			setTimeout(() => tryReg(), 0);
		}
	}
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/**
 * Translate with sidechannel-specific fallbacks.
 * Uses i18n.js t() first; falls back to inline table for mobile-only keys.
 */
const SC_TR = {
	'm.sc.cardLabel':         { tr: 'Araçlar',              en: 'Tools'                },
	'm.sc.clipHint':          { tr: 'Panonuzu uzak cihaza gönderin veya alın.', en: 'Send or receive your clipboard content.' },
	'm.sc.clipReceived':      { tr: 'Uzak pano alındı',     en: 'Remote clipboard received' },
	'm.sc.clipCopy':          { tr: 'Panoya kopyala',        en: 'Copy to clipboard'   },
	'm.sc.clipboardSent':     { tr: 'Pano gönderildi',       en: 'Clipboard sent'      },
	'm.sc.clipboardEmpty':    { tr: 'Pano boş',              en: 'Clipboard is empty'  },
	'm.sc.clipboardError':    { tr: 'Pano okunamadı',        en: 'Could not read clipboard' },
	'm.sc.clipboardCopied':   { tr: 'Kopyalandı',            en: 'Copied'              },
	'm.sc.openChat':          { tr: 'Sohbeti aç',            en: 'Open chat'           },
	'm.sc.unreadAriaLabel':   { tr: '{n} okunmamış mesaj',   en: '{n} unread messages' },
};

function _t(key, vars) {
	// Try i18n.js first
	try {
		const v = t(key, vars);
		if (v !== key) return v;
	} catch (_) {}

	// Fall through to inline table
	const htmlLang = (document.documentElement.lang || 'tr').toLowerCase().slice(0, 2);
	const entry    = SC_TR[key];
	if (!entry) return key;
	let str = (entry[htmlLang] || entry.tr || key);
	if (vars) {
		str = str.replace(/\{(\w+)\}/g, (_, k) => (vars[k] != null ? String(vars[k]) : ''));
	}
	return str;
}

function _escHtml(str) {
	return String(str)
		.replace(/&/g, '&amp;')
		.replace(/</g, '&lt;')
		.replace(/>/g, '&gt;')
		.replace(/"/g, '&quot;')
		.replace(/'/g, '&#39;');
}

function _formatTime(ts) {
	const d = new Date(ts);
	return d.getHours().toString().padStart(2, '0') + ':' +
	       d.getMinutes().toString().padStart(2, '0');
}

// ── Inline styles ─────────────────────────────────────────────────────────────

function _injectStyles() {
	if (document.getElementById('sc-styles')) return;
	const style = document.createElement('style');
	style.id = 'sc-styles';
	style.textContent = `
/* ─────────────────────────────────────────────
   sidechannels.js — clipboard + chat card styles
   (overlay dark surface context)
   ───────────────────────────────────────────── */

/* ── Shared card shell ──────────────────────── */
.sc-card {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 14px 16px 16px;
  background: oklch(1 0 0 / 0.05);
  border: 1px solid oklch(1 0 0 / 0.1);
  border-radius: var(--r, 13px);
}

.sc-card-header {
  display: flex;
  align-items: center;
  gap: 8px;
  min-height: 24px;
}

.sc-card-icon {
  display: flex;
  align-items: center;
  color: var(--brand, oklch(0.555 0.205 272));
  flex: none;
}

.sc-card-title {
  font-size: 13.5px;
  font-weight: 600;
  color: oklch(0.94 0 0);
  flex: 1;
  letter-spacing: -0.01em;
}

.sc-card-body {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.sc-hint {
  font-size: 12px;
  color: oklch(0.58 0 0);
  margin: 0;
  line-height: 1.5;
}

/* ── Primary action button ──────────────────── */
.sc-primary-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  min-height: 44px;
  padding: 0 20px;
  border: none;
  border-radius: var(--r-sm, 9px);
  background: var(--brand, oklch(0.555 0.205 272));
  color: oklch(0.99 0 0);
  font-family: var(--font-sans, sans-serif);
  font-size: 14px;
  font-weight: 600;
  cursor: pointer;
  transition: opacity 0.15s, transform 0.12s;
  touch-action: manipulation;
  width: 100%;
}
.sc-primary-btn:active { opacity: 0.82; transform: scale(0.98); }
.sc-primary-btn:disabled { opacity: 0.45; cursor: not-allowed; }
.sc-primary-btn.loading { opacity: 0.6; }

body[data-mode="game"] .sc-primary-btn {
  background: var(--cyan, oklch(0.62 0.15 215));
}

/* ── Chat card preview ──────────────────────── */
.sc-chat-preview {
  padding: 8px 0 2px;
  min-height: 28px;
}
.sc-no-msg,
.sc-preview-text {
  font-size: 13px;
  color: oklch(0.62 0 0);
  margin: 0;
  line-height: 1.5;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.sc-preview-from {
  color: oklch(0.76 0 0);
  font-weight: 600;
  margin-right: 4px;
}

/* ── Unread badge ───────────────────────────── */
.sc-unread-badge {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 18px;
  height: 18px;
  padding: 0 5px;
  border-radius: var(--r-pill, 999px);
  background: var(--brand, oklch(0.555 0.205 272));
  color: oklch(0.99 0 0);
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0;
  flex: none;
}
body[data-mode="game"] .sc-unread-badge {
  background: var(--cyan, oklch(0.62 0.15 215));
}

/* ── Bottom-sheet backdrop ──────────────────── */
.sc-sheet-backdrop {
  position: fixed;
  inset: 0;
  z-index: 50;
  background: oklch(0 0 0 / 0.4);
  opacity: 0;
  transition: opacity 0.25s var(--ease-out, cubic-bezier(0.16,1,0.3,1));
}
.sc-sheet-backdrop.open { opacity: 1; }

/* ── Bottom-sheet panel ─────────────────────── */
.sc-sheet {
  position: fixed;
  inset: 0;
  z-index: 51;
  pointer-events: none;
  display: flex;
  flex-direction: column;
  justify-content: flex-end;
}

.sc-sheet-panel {
  pointer-events: auto;
  position: relative;
  z-index: 52;
  display: flex;
  flex-direction: column;
  max-height: 80dvh;
  background: oklch(0.16 0.014 268);
  border-top-left-radius: var(--r-xl, 26px);
  border-top-right-radius: var(--r-xl, 26px);
  padding-bottom: calc(env(safe-area-inset-bottom, 0px) + 8px);
  transform: translateY(100%);
  transition: transform 0.3s var(--ease-out, cubic-bezier(0.16,1,0.3,1)),
              bottom 0.15s ease-out;
  box-shadow: 0 -8px 40px oklch(0 0 0 / 0.5);
}
.sc-sheet-panel.open { transform: translateY(0); }

.sc-sheet-handle {
  width: 38px; height: 4px;
  border-radius: 2px;
  background: oklch(1 0 0 / 0.2);
  margin: 12px auto 4px;
  flex: none;
}

.sc-sheet-header {
  display: flex;
  align-items: center;
  padding: 4px 16px 12px;
  flex: none;
  gap: 10px;
}

.sc-sheet-title {
  font-family: var(--font-display, 'Space Grotesk', sans-serif);
  font-size: 16px;
  font-weight: 700;
  letter-spacing: -0.02em;
  color: oklch(0.97 0 0);
  flex: 1;
}

.sc-sheet-close {
  background: oklch(1 0 0 / 0.08);
  color: oklch(0.88 0 0);
  border: none;
  border-radius: var(--r-sm, 9px);
  width: 36px; height: 36px;
  display: flex; align-items: center; justify-content: center;
  cursor: pointer;
  touch-action: manipulation;
  flex: none;
}
.sc-sheet-close:active { opacity: 0.7; }

/* ── Message list ────────────────────────────── */
.sc-msg-list {
  flex: 1;
  overflow-y: auto;
  -webkit-overflow-scrolling: touch;
  padding: 8px 16px 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.sc-empty-msg {
  font-size: 13px;
  color: oklch(0.52 0 0);
  text-align: center;
  padding: 32px 16px;
}

/* ── Message bubbles ─────────────────────────── */
.sc-bubble-row {
  display: flex;
  flex-direction: column;
}
.sc-bubble-row.me  { align-items: flex-end; }
.sc-bubble-row.peer { align-items: flex-start; }

.sc-bubble {
  max-width: 80%;
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 10px 14px;
  border-radius: var(--r, 13px);
  word-break: break-word;
}

.sc-bubble-me {
  background: var(--brand, oklch(0.555 0.205 272));
  border-bottom-right-radius: var(--r-xs, 6px);
}
body[data-mode="game"] .sc-bubble-me {
  background: var(--cyan, oklch(0.62 0.15 215));
}

.sc-bubble-peer {
  background: oklch(1 0 0 / 0.1);
  border: 1px solid oklch(1 0 0 / 0.1);
  border-bottom-left-radius: var(--r-xs, 6px);
}

.sc-bubble-sender {
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.02em;
  color: oklch(1 0 0 / 0.6);
  display: block;
}

.sc-bubble-text {
  font-size: 14px;
  line-height: 1.5;
  color: oklch(0.97 0 0);
  white-space: pre-wrap;
  display: block;
}

.sc-bubble-time {
  font-size: 10.5px;
  color: oklch(1 0 0 / 0.45);
  align-self: flex-end;
  display: block;
}

/* ── Composer ──────────────────────────────────── */
.sc-composer {
  display: flex;
  align-items: flex-end;
  gap: 10px;
  padding: 10px 14px 10px;
  border-top: 1px solid oklch(1 0 0 / 0.1);
  flex: none;
}

.sc-chat-input {
  flex: 1;
  min-height: 44px;
  max-height: 120px;
  padding: 11px 14px;
  border: 1.5px solid oklch(1 0 0 / 0.14);
  border-radius: var(--r-sm, 9px);
  background: oklch(1 0 0 / 0.06);
  color: oklch(0.97 0 0);
  font-family: var(--font-sans, sans-serif);
  font-size: 16px;          /* never below 16px — prevents iOS/Android zoom */
  line-height: 1.45;
  resize: none;
  outline: none;
  touch-action: manipulation;
}
.sc-chat-input::placeholder { color: oklch(0.5 0 0); }
.sc-chat-input:focus {
  border-color: oklch(from var(--brand, oklch(0.555 0.205 272)) l c h / 0.5);
}

.sc-send-btn {
  flex: none;
  width: 44px; height: 44px;
  border-radius: var(--r-sm, 9px);
  border: none;
  background: var(--brand, oklch(0.555 0.205 272));
  color: oklch(0.99 0 0);
  display: flex; align-items: center; justify-content: center;
  cursor: pointer;
  transition: opacity 0.15s, transform 0.1s;
  touch-action: manipulation;
}
.sc-send-btn:active { opacity: 0.78; transform: scale(0.92); }
.sc-send-btn:disabled {
  opacity: 0.3;
  cursor: not-allowed;
}
body[data-mode="game"] .sc-send-btn {
  background: var(--cyan, oklch(0.62 0.15 215));
}

/* ── Clipboard toast ─────────────────────────── */
.sc-toast {
  position: fixed;
  left: 16px;
  right: 16px;
  bottom: calc(env(safe-area-inset-bottom, 0px) + 72px);
  z-index: 200;
  opacity: 0;
  transform: translateY(12px);
  pointer-events: none;
  transition: opacity 0.22s var(--ease-out, cubic-bezier(0.16,1,0.3,1)),
              transform 0.22s var(--ease-out, cubic-bezier(0.16,1,0.3,1));
  border-radius: var(--r, 13px);
}
.sc-toast.visible {
  opacity: 1;
  transform: translateY(0);
  pointer-events: auto;
}

.sc-toast-clip {
  background: oklch(0.2 0.02 268);
  border: 1px solid oklch(1 0 0 / 0.12);
  box-shadow: 0 8px 32px oklch(0 0 0 / 0.5);
  padding: 12px 14px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.sc-toast-body {
  display: flex;
  align-items: center;
  gap: 10px;
  min-height: 24px;
}

.sc-toast-icon {
  flex: none;
  color: var(--brand, oklch(0.555 0.205 272));
}

.sc-toast-text {
  font-size: 13.5px;
  font-weight: 600;
  color: oklch(0.95 0 0);
  flex: 1;
}

.sc-toast-preview {
  display: block;
  font-size: 12px;
  color: oklch(0.62 0 0);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 100%;
}

.sc-toast-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  justify-content: flex-end;
}

.sc-toast-action {
  min-height: 36px;
  padding: 0 14px;
  border: none;
  border-radius: var(--r-sm, 9px);
  background: var(--brand, oklch(0.555 0.205 272));
  color: oklch(0.99 0 0);
  font-family: var(--font-sans, sans-serif);
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  touch-action: manipulation;
}
.sc-toast-action:active { opacity: 0.78; }
body[data-mode="game"] .sc-toast-action {
  background: var(--cyan, oklch(0.62 0.15 215));
}

.sc-toast-dismiss {
  width: 32px; height: 32px;
  border: none;
  border-radius: var(--r-sm, 9px);
  background: oklch(1 0 0 / 0.07);
  color: oklch(0.7 0 0);
  display: flex; align-items: center; justify-content: center;
  cursor: pointer;
  touch-action: manipulation;
}
.sc-toast-dismiss:active { opacity: 0.7; }

/* ── Simple status toast ─────────────────────── */
.sc-toast-status {
  padding: 12px 16px;
  font-size: 14px;
  font-weight: 500;
  color: oklch(0.97 0 0);
  text-align: center;
}
.sc-toast-ok  { background: oklch(0.25 0.06 158); border: 1px solid oklch(0.63 0.15 158 / 0.4); }
.sc-toast-warn { background: oklch(0.22 0.04 75);  border: 1px solid oklch(0.72 0.15 75 / 0.4); }
.sc-toast-err  { background: oklch(0.22 0.05 25);  border: 1px solid oklch(0.575 0.205 25 / 0.4); }
`;
	document.head.appendChild(style);
}

// ── Boot ──────────────────────────────────────────────────────────────────────

_injectStyles();
_wireBus();
_register();
