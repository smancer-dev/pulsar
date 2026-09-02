//! Headless relay mode. `pulsar --relay [--bind H:P | --host H --port P]` runs the
//! rendezvous/relay server instead of the GUI, so the same install can self-host a
//! relay. Extracted from `lib.rs` to keep that file focused (see PENDING-WORK #9).
//!
//! Note: on Windows the app is built with the GUI subsystem (no console), so relay
//! logs won't show in a terminal — use the standalone `pulsar-relay` binary there.

use std::net::SocketAddr;

/// Parse the relay CLI args and run the relay loop forever (blocks).
pub fn run_relay(args: &[String]) {
	let flag = |name: &str| {
		args.iter()
			.position(|a| a == name)
			.and_then(|i| args.get(i + 1))
			.cloned()
	};
	let addr: SocketAddr = match flag("--bind") {
		Some(b) => b
			.parse()
			.expect("invalid --bind address (expected host:port)"),
		None => {
			let host = flag("--host").unwrap_or_else(|| "0.0.0.0".into());
			let port = flag("--port")
				.unwrap_or_else(|| pulsar_core::proto::DEFAULT_RELAY_PORT.to_string());
			format!("{host}:{port}")
				.parse()
				.expect("invalid --host/--port")
		}
	};
	// 2FA enrollment: `pulsar --relay --generate-totp` prints a scannable QR + the
	// manual-entry code and exits WITHOUT starting the relay, so an operator never has to
	// invent a base32 secret by hand. Same output as the standalone binary's flag.
	// (Windows: this build has no console — use the standalone `pulsar-relay` there.)
	if args.iter().any(|a| a == "--generate-totp") {
		let label = addr.to_string();
		// Re-display an existing secret if one was passed, else mint a fresh one — so an
		// operator can recover a lost QR without invalidating everyone's enrollment.
		let e = match flag("--auth-totp").or_else(|| std::env::var("PULSAR_RELAY_AUTH_TOTP").ok()) {
			Some(s) => {
				pulsar_relay::base32_decode(&s)
					.filter(|b| !b.is_empty())
					.unwrap_or_else(|| {
						panic!("invalid --auth-totp (expected a base32 secret): {s:?}")
					});
				pulsar_relay::enrollment_from_secret(&s, &label, "Pulsar")
			}
			None => pulsar_relay::generate_totp(&label, "Pulsar"),
		};
		println!("{}", e.qr_terminal);
		println!("Pulsar relay 2FA (TOTP) enrollment");
		println!("  Scan the QR above with any authenticator app (Google Authenticator, Aegis, 1Password, …).");
		println!("  Can't scan? Enter this code by hand:");
		println!("      {}", e.secret_grouped);
		println!("  Setup URI: {}", e.uri);
		println!();
		println!("  Then start the relay with:");
		println!("      pulsar --relay --auth-totp {}", e.secret);
		println!("  (add --auth-password <pw> to require BOTH factors)");
		return;
	}
	// Operator bandwidth limits (same flags + env as the standalone `pulsar-relay`).
	// Default unlimited; e.g. `pulsar --relay --user-rate 10mbit --user-data 5gb`.
	let rate = |name: &str, env: &str| {
		flag(name).or_else(|| std::env::var(env).ok()).map(|s| {
			pulsar_relay::parse_rate(&s).unwrap_or_else(|| panic!("invalid {name}: {s:?}"))
		})
	};
	let size = |name: &str, env: &str| {
		flag(name).or_else(|| std::env::var(env).ok()).map(|s| {
			pulsar_relay::parse_size(&s).unwrap_or_else(|| panic!("invalid {name}: {s:?}"))
		})
	};
	let limits = pulsar_relay::Limits {
		per_user_bps: rate("--user-rate", "PULSAR_RELAY_USER_RATE"),
		per_user_total: size("--user-data", "PULSAR_RELAY_USER_DATA"),
		per_ip_bps: rate("--ip-rate", "PULSAR_RELAY_IP_RATE"),
		per_ip_total: size("--ip-data", "PULSAR_RELAY_IP_DATA"),
		per_session_bps: rate("--session-rate", "PULSAR_RELAY_SESSION_RATE"),
		per_session_total: size("--session-data", "PULSAR_RELAY_SESSION_DATA"),
	};
	// Operator auth policy (v4). Default: open relay. A base32 2FA secret is decoded up
	// front so a typo fails fast at startup rather than silently rejecting every login.
	// `pulsar --relay --auth-password s3cret --auth-totp JBSWY3DPEHPK3PXP --require-e2e`.
	let auth_password = flag("--auth-password").or_else(|| std::env::var("PULSAR_RELAY_AUTH_PASSWORD").ok());
	let totp_secret = flag("--auth-totp")
		.or_else(|| std::env::var("PULSAR_RELAY_AUTH_TOTP").ok())
		.map(|s| {
			pulsar_relay::base32_decode(&s)
				.filter(|b| !b.is_empty())
				.unwrap_or_else(|| panic!("invalid --auth-totp (expected a base32 secret): {s:?}"))
		});
	let require_e2e = args.iter().any(|a| a == "--require-e2e")
		|| std::env::var("PULSAR_RELAY_REQUIRE_E2E").is_ok();
	let auth = pulsar_relay::RelayAuth {
		password: auth_password,
		totp_secret,
		require_e2e,
	};
	let auth_summary = (
		auth.password.is_some(),
		auth.totp_secret.is_some(),
		auth.require_e2e,
	);
	tracing_subscriber::fmt()
		.with_env_filter(
			tracing_subscriber::EnvFilter::try_from_default_env()
				.unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
		)
		.init();
	let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
	rt.block_on(async move {
		let relay = pulsar_relay::Relay::bind(addr)
			.await
			.expect("relay failed to bind")
			.with_limits(limits)
			.with_auth(auth);
		tracing::info!(
			%addr, ?limits,
			password = auth_summary.0, totp = auth_summary.1, require_e2e = auth_summary.2,
			"Pulsar relay listening (UDP) — headless --relay mode"
		);
		relay.run().await.expect("relay loop exited with error");
	});
}
