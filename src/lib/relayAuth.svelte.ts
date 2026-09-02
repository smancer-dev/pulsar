// Shared relay-authentication state (v4). The relay password lives in the persisted
// config; this holds the transient pieces the register flow and the Network settings UI
// exchange: a one-shot 2FA code, which factors the relay is asking for, whether the last
// attempt was rejected, and whether the relay advertises E2E as required.
//
// It's a tiny module-level rune store so NetworkTab (deep in the Settings tree) and the
// shell's goOnline() can share it without threading callbacks through every layer.
export const relayAuth = $state({
	/** One-shot TOTP/2FA code the user typed; consumed (cleared) on each go_online attempt. */
	totp: '',
	/** The relay asked for a password we didn't have (from a RELAY_AUTH_REQUIRED reply). */
	needPassword: false,
	/** The relay asked for a 2FA code we didn't have. */
	needTotp: false,
	/** The last attempt supplied a credential the relay rejected (wrong password / 2FA). */
	failed: false,
	/** The relay advertises end-to-end encryption as required (drives the lock badge). */
	e2eRequired: false
});

/** Note a RELAY_AUTH_REQUIRED:<password>:<totp> error string from go_online. */
export function markAuthRequired(password: boolean, totp: boolean) {
	relayAuth.needPassword = password;
	relayAuth.needTotp = totp;
	relayAuth.failed = false;
}

/** Note a rejected credential (RELAY_AUTH_FAILED). */
export function markAuthFailed() {
	relayAuth.failed = true;
}

/** A successful registration clears the transient auth prompts (keeps e2eRequired,
 * which the relay-e2e event owns). */
export function clearAuthPrompts() {
	relayAuth.needPassword = false;
	relayAuth.needTotp = false;
	relayAuth.failed = false;
}
