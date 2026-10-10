//! FIDO2 hardware key authentication via hmac-secret extension.
//!
//! Provides credential registration and assertion using a FIDO2
//! authenticator's hmac-secret extension. The 32-byte output from
//! assertion feeds into `qpv2_core::utilities::derive_vault_enc_key()`
//! to produce the AES-256 vault encryption key.
//!
//! Security model: short PIN verified on-device (8 retries before
//! permanent lockout) + hardware HMAC = 256-bit derived key.
//! PIN brute-force is physically rate-limited by the authenticator.
//!
//! The PIN reaches the authenticator in one of two ways (see [`PinMode`]):
//! typed on the host and sent over CTAP clientPin (e.g. YubiKey), or
//! entered on the authenticator itself (e.g. Trezor, which does not
//! implement clientPin and rejects a host PIN with `UNSUPPORTED_OPTION`).

use ctap_hid_fido2::fidokey::get_assertion::Extension as Gext;
use ctap_hid_fido2::fidokey::make_credential::Extension as Mext;
use ctap_hid_fido2::fidokey::GetAssertionArgsBuilder;
use ctap_hid_fido2::fidokey::MakeCredentialArgsBuilder;
use ctap_hid_fido2::{get_fidokey_devices, FidoKeyHidFactory, HidInfo, LibCfg};
use qpv2_core::{utilities, SecureVec};
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

/// How the connected authenticator verifies its PIN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinMode {
    /// The PIN is typed on the host and sent over CTAP clientPin.
    Host,
    /// The PIN is entered on the authenticator itself (built-in user
    /// verification); the host must not send one.
    OnDevice,
}

/// Enumerate connected FIDO2 authenticators.
pub fn list_devices() -> Vec<HidInfo> {
    get_fidokey_devices()
}

/// Ask the connected authenticator how it takes its PIN, so callers know
/// whether to prompt for one. Fails if no authenticator is connected.
pub fn pin_mode() -> Result<PinMode, String> {
    let cfg = LibCfg::init();
    let device = FidoKeyHidFactory::create(&cfg).map_err(map_err)?;
    let info = device.get_info().map_err(map_err)?;
    pin_mode_from_options(&info.options)
}

/// clientPin present (set or not) means the host supplies the PIN; keep
/// that path even when built-in UV also exists (e.g. YubiKey Bio), so
/// existing credentials keep working unchanged.
fn pin_mode_from_options(options: &[(String, bool)]) -> Result<PinMode, String> {
    let option = |name: &str| options.iter().find(|(n, _)| n == name).map(|(_, v)| *v);
    if option("clientPin").is_some() {
        Ok(PinMode::Host)
    } else if option("uv") == Some(true) {
        Ok(PinMode::OnDevice)
    } else {
        Err("This security key cannot verify a PIN, so it cannot protect a wallet.".to_string())
    }
}

/// Register a new credential with hmac-secret extension enabled.
///
/// Requires user interaction: touch the security key when it blinks.
/// `pin` is the authenticator's client PIN for [`PinMode::Host`], or
/// `None` for [`PinMode::OnDevice`], where the key asks for it itself.
///
/// Returns the credential ID to persist in `meta.json`.
pub fn register(pin: Option<&str>) -> Result<Fido2Credential, String> {
    let cfg = LibCfg::init();
    let device = FidoKeyHidFactory::create(&cfg).map_err(map_err)?;

    let challenge = rand_challenge()?;

    let mut builder = MakeCredentialArgsBuilder::new(RP_ID, &challenge)
        .extensions(&[Mext::HmacSecret(Some(true))]);
    if let Some(pin) = pin {
        builder = builder.pin(pin);
    }
    let mut args = builder.build();
    if pin.is_none() {
        // Without a host PIN, require on-device user verification.
        args.uv = Some(true);
    }

    let attestation = device.make_credential_with_args(&args).map_err(map_err)?;

    Ok(Fido2Credential {
        credential_id: attestation.credential_descriptor.id,
    })
}

/// Authenticate with an existing credential using hmac-secret.
///
/// Requires user interaction: touch the security key when it blinks.
/// `pin` follows the same rule as in [`register`].
/// Returns the 32-byte hmac-secret output suitable for
/// `qpv2_core::utilities::derive_vault_enc_key()`.
pub fn authenticate(credential_id: &[u8], pin: Option<&str>) -> Result<SecureVec, String> {
    let cfg = LibCfg::init();
    let device = FidoKeyHidFactory::create(&cfg).map_err(map_err)?;

    let challenge = rand_challenge()?;

    let mut builder = GetAssertionArgsBuilder::new(RP_ID, &challenge)
        .credential_id(credential_id)
        .extensions(&[Gext::HmacSecret(Some(HMAC_SALT))]);
    if let Some(pin) = pin {
        builder = builder.pin(pin);
    }
    let mut args = builder.build();
    if pin.is_none() {
        // Without a host PIN, require on-device user verification.
        args.uv = Some(true);
    }

    let assertions = device.get_assertion_with_args(&args).map_err(map_err)?;
    let assertion = assertions
        .into_iter()
        .next()
        .ok_or_else(|| "No assertion returned from authenticator.".to_string())?;

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

fn map_err(e: impl std::fmt::Display) -> String {
    let msg = e.to_string();
    if msg.contains("Cancelled") || msg.contains("cancel") {
        "Cancelled.".to_string()
    } else if msg.contains("PIN") && msg.contains("invalid") {
        "PIN verification failed.".to_string()
    } else if msg.contains("PIN") && msg.contains("locked") {
        "Authenticator locked — too many PIN attempts.".to_string()
    } else if msg.contains("not found") || msg.contains("No device") {
        "No FIDO2 security key detected.".to_string()
    } else {
        format!("FIDO2 error: {}", msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(list: &[(&str, bool)]) -> Vec<(String, bool)> {
        list.iter().map(|(n, v)| (n.to_string(), *v)).collect()
    }

    #[test]
    fn client_pin_key_uses_host_pin() {
        // YubiKey 5: clientPin, no built-in UV.
        let o = opts(&[("rk", true), ("up", true), ("clientPin", true)]);
        assert_eq!(pin_mode_from_options(&o), Ok(PinMode::Host));
    }

    #[test]
    fn client_pin_wins_over_builtin_uv() {
        // YubiKey Bio: both; stay on the host-PIN path.
        let o = opts(&[("uv", true), ("clientPin", true)]);
        assert_eq!(pin_mode_from_options(&o), Ok(PinMode::Host));
    }

    #[test]
    fn builtin_uv_only_key_uses_on_device_pin() {
        // Trezor: uv but no clientPin.
        let o = opts(&[("rk", true), ("up", true), ("uv", true)]);
        assert_eq!(pin_mode_from_options(&o), Ok(PinMode::OnDevice));
    }

    #[test]
    fn key_without_pin_support_is_rejected() {
        let o = opts(&[("rk", false), ("up", true)]);
        assert!(pin_mode_from_options(&o).is_err());
    }
}
