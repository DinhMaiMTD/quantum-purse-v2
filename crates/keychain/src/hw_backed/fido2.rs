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
use ctap_hid_fido2::{get_fidokey_devices, FidoKeyHidFactory, HidInfo, LibCfg};
use qpv2_core::{utilities, SecureString, SecureVec};
use zeroize::Zeroize;

const RP_ID: &str = "quantumpurse.org";

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
    let cfg = LibCfg::init();
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
    let cfg = LibCfg::init();
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

    let attestation = device
        .make_credential_with_args(&args)
        .map_err(|e| map_err_for(verification, e))?;

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
    let cfg = LibCfg::init();
    let device = FidoKeyHidFactory::create(&cfg).map_err(map_err)?;

    let challenge = rand_challenge()?;

    let builder = GetAssertionArgsBuilder::new(RP_ID, &challenge)
        .credential_id(credential_id)
        .extensions(&[Gext::HmacSecret(Some(HMAC_SALT))]);
    let args = match verification {
        Verification::OnHost(pin) => builder.pin(pin).build(),
        Verification::OnDevice => builder.build(),
    };

    let assertions = device
        .get_assertion_with_args(&args)
        .map_err(|e| map_err_for(verification, e))?;
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

fn map_err(e: impl std::fmt::Display) -> String {
    let msg = e.to_string();
    if msg.contains("Cancelled") || msg.contains("cancel") {
        "Cancelled.".to_string()
    } else if msg.contains("PIN") && msg.contains("invalid") {
        "PIN verification failed.".to_string()
    } else if msg.contains("PIN") && msg.contains("locked") {
        "Authenticator locked — too many PIN attempts.".to_string()
    } else if msg.contains("not found") || msg.contains("No device") {
        "No FIDO2 security key detected. Is your device connected and unlocked?".to_string()
    } else {
        format!("FIDO2 error: {}", msg)
    }
}
