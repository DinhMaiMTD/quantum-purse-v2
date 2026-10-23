//! FIDO2 hardware key authentication via hmac-secret extension.
//!
//! Provides credential registration and assertion using a FIDO2
//! authenticator's hmac-secret extension. The 32-byte output from
//! assertion feeds into `qpv2_core::utilities::derive_vault_enc_key()`
//! to produce the AES-256 vault encryption key.
//!
//! Security model: the key verifies its user (a PIN sent from this host, or
//! the key's own screen, keypad or sensor) + hardware HMAC = 256-bit derived
//! key. PIN brute-force is physically rate-limited by the authenticator.

use ctap_hid_fido2::fidokey::get_assertion::Extension as Gext;
use ctap_hid_fido2::fidokey::get_info::InfoOption;
use ctap_hid_fido2::fidokey::make_credential::Extension as Mext;
use ctap_hid_fido2::fidokey::GetAssertionArgsBuilder;
use ctap_hid_fido2::fidokey::MakeCredentialArgsBuilder;
use ctap_hid_fido2::public_key_credential_user_entity::PublicKeyCredentialUserEntity;
use ctap_hid_fido2::{get_fidokey_devices, FidoKeyHid, FidoKeyHidFactory, HidInfo, LibCfg};
use qpv2_core::{utilities, SecureString, SecureVec};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;
use zeroize::Zeroize;

const RP_ID: &str = "quantumpurse.org";

/// The open USB connection to the security key while a request waits on the
/// user, kept where [`cancel`] can reach it from another thread; empty
/// between requests. One at a time: a second request would reach the key
/// while it waits for the first one's touch, so callers send them one by one.
static USB_CONNECTION: Mutex<Option<Arc<FidoKeyHid>>> = Mutex::new(None);

/// How long a request may wait on the user before it is cancelled. The
/// library stops listening on its own after at least 50 s and then misreports
/// the key's "still waiting" status as `CTAP1_ERR_INVALID_PARAMETER`, so this
/// stays under that. It leaves time for a PIN entered on the key's own screen.
const USER_WAIT_LIMIT: Duration = Duration::from_secs(45);

const NOT_TOUCHED_IN_TIME: &str =
    "Security key wasn't touched in time. Retry and touch it when it blinks.";

/// Fixed 32-byte salt for hmac-secret. Derived conceptually from
/// "quantum-purse-v2/fido2/hmac-salt/v1" but hardcoded to avoid
/// runtime derivation. Changing this invalidates all existing
/// FIDO2 credentials.
const HMAC_SALT: [u8; 32] = [
    0x71, 0x75, 0x61, 0x6e, 0x74, 0x75, 0x6d, 0x2d, // "quantum-"
    0x70, 0x75, 0x72, 0x73, 0x65, 0x2d, 0x76, 0x32, // "purse-v2"
    0x2f, 0x66, 0x69, 0x64, 0x6f, 0x32, 0x2f, 0x68, // "/fido2/h"
    0x6d, 0x61, 0x63, 0x2d, 0x73, 0x61, 0x6c, 0x74, // "mac-salt"
];

/// Registered FIDO2 credential.
pub struct Fido2Credential {
    pub credential_id: Vec<u8>,
}

/// How the connected key verifies its user. Keys state this in their
/// capability list; the two kinds need different requests, and a key of one
/// kind rejects the other's.
pub enum Verification {
    /// The key has no input IO (e.g. a YubiKey 5): this host collects
    /// the key's PIN and sends it along.
    OnHost(SecureString),
    /// The key verifies on itself: a Trezor's screen and PIN, a fingerprint
    /// sensor, a keypad.
    OnDevice,
}

/// Enumerate connected FIDO2 authenticators.
pub fn list_devices() -> Vec<HidInfo> {
    get_fidokey_devices()
}

/// Makes the app's first USB scan, from the main thread. Call once at startup.
///
/// On macOS, hidapi (C) ties its global device manager to the event loop of
/// the first thread that scans, for the life of the process. A short-lived
/// worker would leave it pointing at a freed event loop, and a later scan can
/// then crash the app. Tying it to the main thread avoids that. Elsewhere
/// there is no such tie, so this does nothing.
pub fn init_usb_on_main_thread() {
    #[cfg(target_os = "macos")]
    {
        // The FIDO2 library's own scan, so it starts the same hidapi. It
        // panics if hidapi can't start; catching it keeps launch independent
        // of FIDO2, though any later FIDO2 use panics the same way.
        let _ = std::panic::catch_unwind(get_fidokey_devices);
    }
}

/// The library's settings, with its own touch prompt turned off: it prints a
/// fixed, plain line to stdout whenever the key waits, which callers can't
/// style or place. Callers say what to do themselves (the CLI on the
/// terminal, the app on screen).
fn lib_cfg() -> LibCfg {
    let mut cfg = LibCfg::init();
    cfg.enable_keep_alive_msg = false;
    cfg
}

/// Keys only have to accept names this long; longer ones may be refused.
const LABEL_MAX_BYTES: usize = 64;

/// Asks the connected key how it verifies its user, so the caller knows
/// whether to prompt for a PIN. `Ok(true)` means prompt; `Ok(false)` means
/// the key handles it.
///
/// A key that offers both (a fingerprint key with a PIN as fallback) gets the
/// host PIN: CTAP 2.1 deprecates the plain `uv` request in favour of a
/// token flow the library lacks, and the PIN path is the one proven to work.
/// On-device is used only where it is the key's sole method, as on a Trezor.
pub fn needs_host_pin() -> Result<bool, String> {
    let cfg = lib_cfg();
    let device = FidoKeyHidFactory::create(&cfg).map_err(map_err)?;
    let info = device.get_info().map_err(map_err)?;
    // Present and enabled is `Some(true)`; present but off is `Some(false)`.
    let option = |name: &InfoOption| {
        info.options
            .iter()
            .find(|(k, _)| k == name.as_ref())
            .map(|(_, v)| *v)
    };
    match (option(&InfoOption::ClientPin), option(&InfoOption::Uv)) {
        (Some(true), _) => Ok(true),
        (_, Some(true)) => Ok(false),
        (Some(false), _) | (_, Some(false)) => Err(
            "The security key has no way to verify you yet: set a PIN or enroll a fingerprint \
             with its vendor tool first."
                .to_string(),
        ),
        (None, None) => {
            Err("The security key verifies users in no way this wallet supports.".to_string())
        }
    }
}

/// Register a new credential with hmac-secret extension enabled.
///
/// Requires user interaction: touch the security key when it blinks, or
/// confirm on the key's own screen.
///
/// `label` names the credential on keys with a screen, e.g. the wallet's
/// name on a Trezor. It is fixed at registration: renaming the wallet later
/// does not change it.
///
/// Returns the credential ID to persist in `meta.json`.
pub fn register(verification: &Verification, label: &str) -> Result<Fido2Credential, String> {
    let cfg = lib_cfg();
    let device = FidoKeyHidFactory::create(&cfg).map_err(map_err)?;

    let challenge = rand_challenge()?;

    let label = truncate_label(label);
    let user = PublicKeyCredentialUserEntity::new(None, Some(label), Some(label));
    let builder = MakeCredentialArgsBuilder::new(RP_ID, &challenge)
        .user_entity(&user)
        .extensions(&[Mext::HmacSecret(Some(true))]);
    // The builder starts with no PIN and the `uv` option set, which is the
    // on-device request; `.pin()` switches it to the host-PIN request.
    let args = match verification {
        Verification::OnHost(pin) => builder.pin(pin).build(),
        Verification::OnDevice => builder.build(),
    };

    let attestation = run_cancellable_request(device, verification, |device| {
        device.make_credential_with_args(&args)
    })?;

    Ok(Fido2Credential {
        credential_id: attestation.credential_descriptor.id,
    })
}

/// Cuts `label` to [`LABEL_MAX_BYTES`] on a character boundary.
fn truncate_label(label: &str) -> &str {
    let mut end = label.len().min(LABEL_MAX_BYTES);
    while !label.is_char_boundary(end) {
        end -= 1;
    }
    &label[..end]
}

/// Authenticate with an existing credential using hmac-secret.
///
/// Requires user interaction: touch the security key when it blinks, or
/// confirm on the key's own screen. Returns the 32-byte hmac-secret output
/// suitable for `qpv2_core::utilities::derive_vault_enc_key()`.
pub fn authenticate(
    credential_id: &[u8],
    verification: &Verification,
) -> Result<SecureVec, String> {
    let cfg = lib_cfg();
    let device = FidoKeyHidFactory::create(&cfg).map_err(map_err)?;

    let challenge = rand_challenge()?;

    let builder = GetAssertionArgsBuilder::new(RP_ID, &challenge)
        .credential_id(credential_id)
        .extensions(&[Gext::HmacSecret(Some(HMAC_SALT))]);
    let args = match verification {
        Verification::OnHost(pin) => builder.pin(pin).build(),
        Verification::OnDevice => builder.build(),
    };

    let assertions = run_cancellable_request(device, verification, |device| {
        device.get_assertion_with_args(&args)
    })?;
    let assertion = assertions
        .into_iter()
        .next()
        .ok_or_else(|| "No assertion returned from authenticator.".to_string())?;

    // The key reports whether it actually verified the user. Trust that
    // report, not the capability it advertised: a key that skipped
    // verification must not hand out the vault key.
    if !assertion.flags.user_verified_result {
        return Err("The security key did not verify you.".to_string());
    }

    let mut hmac_output = assertion
        .extensions
        .into_iter()
        .find_map(|ext| match ext {
            Gext::HmacSecret(Some(bytes)) => Some(bytes),
            _ => None,
        })
        .ok_or_else(|| "Authenticator did not return hmac-secret output.".to_string())?;

    // `hmac_output` is a fixed `[u8; 32]` (Copy) living on the stack, so
    // `to_vec` copies it into the zeroizing `SecureVec`. Wipe the stack
    // copy afterwards so the secret does not linger past return.
    let secure = SecureVec::from_vec(hmac_output.to_vec());
    hmac_output.zeroize();
    Ok(secure)
}

/// True while a request waits on the user, for a touch or for the key's own
/// screen, keypad or sensor, so [`cancel`] can stop it. A caller shows its
/// Cancel only then; the PIN dialog before it has its own.
pub fn cancellable() -> bool {
    usb_connection().is_some()
}

/// Stops the request that is waiting on the user, if any: the key stops
/// blinking and answers the request with `CTAP2_ERR_KEEPALIVE_CANCEL`, which
/// its thread reports as "Cancelled.". Returns once the cancel has been sent.
pub fn cancel() -> Result<(), String> {
    let Some(device) = usb_connection().clone() else {
        return Ok(());
    };
    // CTAPHID_CANCEL on the request's channel. The library names it after
    // its selection command, but it cancels whatever request is pending.
    device
        .cancel_selection()
        .map_err(|e| format!("Could not cancel the security key's request: {}", e))
}

/// Runs `request`, which waits on the user, where [`cancel`] can stop it, and
/// cancels it at [`USER_WAIT_LIMIT`].
fn run_cancellable_request<T, E: std::fmt::Display>(
    device: FidoKeyHid,
    verification: &Verification,
    request: impl FnOnce(&FidoKeyHid) -> Result<T, E>,
) -> Result<T, String> {
    let device = Arc::new(device);
    // The timer cancels the request at the limit; the request ending drops
    // `done`, which wakes the timer early so it exits without cancelling.
    let (done, wait) = mpsc::channel::<()>();
    let timer_device = Arc::clone(&device);
    let timer = thread::Builder::new()
        .name("fido2-wait-limit".to_string())
        .spawn(move || {
            let expired = matches!(
                wait.recv_timeout(USER_WAIT_LIMIT),
                Err(RecvTimeoutError::Timeout)
            );
            if expired {
                let _ = timer_device.cancel_selection();
            }
            expired
        })
        .map_err(|e| format!("Failed to start the security key's timer: {}", e))?;

    *usb_connection() = Some(Arc::clone(&device));
    let result = request(&device);
    *usb_connection() = None;
    drop(done);
    let timed_out = timer.join().unwrap_or(false);

    result.map_err(|e| {
        if timed_out {
            NOT_TOUCHED_IN_TIME.to_string()
        } else {
            map_err_for(verification, e)
        }
    })
}

fn usb_connection() -> MutexGuard<'static, Option<Arc<FidoKeyHid>>> {
    // A panic while this was locked leaves nothing half-written in it.
    USB_CONNECTION.lock().unwrap_or_else(|e| e.into_inner())
}

fn rand_challenge() -> Result<[u8; 32], String> {
    let bytes = utilities::get_random_bytes(32).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&bytes);
    Ok(buf)
}

/// `map_err` plus the one failure only an on-device request produces: the
/// key advertised on-device verification but has nothing set up for it. A
/// Trezor does this when it has no PIN (it reports `uv` regardless) and
/// answers `CTAP2_ERR_UNSUPPORTED_OPTION`.
fn map_err_for(verification: &Verification, e: impl std::fmt::Display) -> String {
    let msg = e.to_string();
    if matches!(verification, Verification::OnDevice) && msg.contains("UNSUPPORTED_OPTION") {
        return "The security key could not verify you on the device. On a Trezor, set a device \
                PIN first."
            .to_string();
    }
    map_err(msg)
}

/// Error translation table.
const ERR_MSGS: [(&str, &str); 7] = [
    (
        "CTAP2_ERR_OPERATION_DENIED",
        "The security key wasn't touched in time, or the request was declined on it. \
         Try again and touch it when it blinks.",
    ),
    // Some keys stop waiting on their own before `USER_WAIT_LIMIT`.
    ("CTAP2_ERR_USER_ACTION_TIMEOUT", NOT_TOUCHED_IN_TIME),
    // The key's answer to a request stopped by `cancel`.
    ("CTAP2_ERR_KEEPALIVE_CANCEL", "Cancelled."),
    ("CTAP2_ERR_PIN_INVALID", "Wrong PIN."),
    // Three wrong PINs in a row: the key refuses PINs until it is power-cycled.
    (
        "CTAP2_ERR_PIN_AUTH_BLOCKED",
        "Too many wrong PINs in a row. Unplug the security key, plug it back in, and try again.",
    ),
    // Out of PIN retries: only a reset unblocks the key, and a reset deletes
    // its credentials, this wallet's included.
    (
        "CTAP2_ERR_PIN_BLOCKED",
        "The security key's PIN is blocked after too many wrong tries. Only a reset unblocks \
         it, and a reset erases this wallet's credential, so you will need the seed phrase.",
    ),
    (
        "CTAP2_ERR_NO_CREDENTIALS",
        "This security key doesn't hold this wallet's credential. Plug in the key the wallet \
         was created with.",
    ),
];

fn map_err(e: impl std::fmt::Display) -> String {
    let msg = e.to_string();
    if let Some((_, said)) = ERR_MSGS.iter().find(|(code, _)| msg.contains(code)) {
        said.to_string()
    } else if msg.contains("Multiple FIDO devices") {
        "More than one security key is plugged in. Unplug all but this wallet's key.".to_string()
    } else if msg.contains("not found") || msg.contains("No device") {
        "No FIDO2 security key detected. Is your device connected and unlocked?".to_string()
    } else {
        format!("FIDO2 error: {}", msg)
    }
}
