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
	FsEntry,
	FsEntries,
	SessionEvent,
	AuthPrompt
} from './api.types';

export { isTauri } from './api.invoke';

export type { NavInput } from './api.events';

export { api } from './api.commands';

export {
	onLocalCaps,
	onGamepadNav,
	onChatMsg,
	onHostChat,
	onClipboardIn,
	onDataClip,
	onFileRecv,
	onFileBegin,
	onFsEntries,
	onPeerAvatar,
	onPeerName,
	onPeerId,
	onKbdLeave,
	onKbdEngaged,
	onKbdReleased,
	onOverlayToggle,
	onFullscreenToggle,
	onOverlayCmd,
	onOverlayEnd,
	onOverlayClose,
	onOverlayChat,
	onOverlayFs,
	onOverlayFiles,
	onWindowBlur,
	onPlayEnded,
	onPlayReady,
	onReverseRequest,
	onConnPhase,
	onPlayRtt,
	onPlayStats,
	onPlayVStats,
	onPlayDecoder,
	onPlayDims,
	onSessionEvent,
	onAuthPrompt,
	onNodePort,
	onGuideToggle,
	onControllerConnected,
	onNodeId,
	onNodeVersionError,
	onSessionPassword
} from './api.events';

export { copyText, setFullscreen, readClipboard, windowControl } from './api.dom';
