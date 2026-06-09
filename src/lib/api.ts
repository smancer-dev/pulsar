// Bridge between the SvelteKit UI and the Rust core (pulsar-core) exposed via
// Tauri commands. When running outside Tauri (e.g. `vite dev` in a browser, or
// component tests) it falls back to a deterministic mock so the UI is fully
// usable without the native shell.
//
// This file is a barrel: the implementation lives in cohesive sibling modules
// (`api.types`, `api.invoke`, `api.commands`, `api.events`, `api.dom`) and is
// re-exported here so every `import { … } from '$lib/api'` is unaffected.

export type {
	ScannedApp,
	GameInfo,
	LanDevice,
	PlayInfo,
	DataText,
	FileRecv,
	SessionEvent,
	AuthPrompt
} from './api.types';

export { isTauri } from './api.invoke';

export { api } from './api.commands';

export {
	onChatMsg,
	onHostChat,
	onClipboardIn,
	onDataClip,
	onFileRecv,
	onKbdLeave,
	onOverlayToggle,
	onFullscreenToggle,
	onOverlayCmd,
	onOverlayEnd,
	onOverlayClose,
	onWindowBlur,
	onPlayEnded,
	onReverseRequest,
	onConnPhase,
	onPlayRtt,
	onPlayStats,
	onPlayVStats,
	onSessionEvent,
	onAuthPrompt
} from './api.events';

export { copyText, setFullscreen, readClipboard, windowControl } from './api.dom';
