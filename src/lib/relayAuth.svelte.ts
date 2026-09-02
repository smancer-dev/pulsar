// Relay-authentication state (v5).
//
// The design is deliberately NOT "fields in Settings". A relay's credentials are asked
// for ONCE, in a prompt, at the moment the relay actually demands them — and the answer
// is never stored. What IS stored is the durable ACCESS KEY the relay issues in return
// (in the Rust config, keyed by relay address), so every later launch registers silently.
// The prompt therefore reappears only when the operator rotates the relay's credentials,
// which invalidates the key.
//
// This tiny module-level rune store is what the shell's goOnline() and the prompt dialog
// share, without threading callbacks through every layer.

export const relayAuth = $state({
	/** The prompt is open. */
	open: false,
	/** The relay is asking for a password. */
	needPassword: false,
	/** The relay is asking for a 2FA code. */
	needTotp: false,
	/** The previous answer was rejected (wrong password / 2FA) — shown inside the prompt. */
	failed: false,
	/** A retry is in flight (registration is being attempted with what was typed). */
	busy: false,
	/** The relay address the prompt is for, shown so the user knows what they're unlocking. */
	relay: '',
	/** The relay advertises end-to-end encryption as required (drives the lock badge). */
	e2eRequired: false,
	/** Set once a key has been stored for this relay — Settings shows "bu cihaz yetkili". */
	authorized: false
});

/** Open the prompt for a `RELAY_AUTH_REQUIRED:<password>:<totp>` reply from go_online. */
export function promptRelayAuth(password: boolean, totp: boolean, relay: string) {
	relayAuth.needPassword = password;
	relayAuth.needTotp = totp;
	relayAuth.relay = relay;
	relayAuth.failed = false;
	relayAuth.busy = false;
	relayAuth.open = true;
}

/** The credential just tried was rejected — keep the prompt open and say so. */
export function markRelayAuthFailed() {
	relayAuth.failed = true;
	relayAuth.busy = false;
	relayAuth.open = true;
}

/** Registration succeeded: close the prompt. The access key is now stored by the backend,
 * so this device won't be asked again. */
export function clearRelayAuth() {
	relayAuth.open = false;
	relayAuth.failed = false;
	relayAuth.busy = false;
	relayAuth.needPassword = false;
	relayAuth.needTotp = false;
	relayAuth.authorized = true;
}
