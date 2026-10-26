//! Multisig co-signing logic: collecting co-signers' responses on the
//! initiating side, and answering someone else's signing request on the
//! co-signing side. The screens that drive it are in `ui/cosigner.rs`.

use qpv2_core::types::{AuthKey, AuthMethod, SigningRequest, SigningResponse, SpxVariant};
use qpv2_core::KeyVault;

use crate::types::{Status, TransactionStatus};
use crate::wallet::{get_fido2_dek, spawn_fido2_job};
use crate::App;

/// Who in this wallet signs a co-signer's request, and what.
struct CosignSigner {
    variant: SpxVariant,
    /// This wallet's position among the request's signers.
    signer_index: usize,
    /// The single-sig account that signs.
    lock_args: String,
    /// The message to sign, decoded from the request.
    message: Vec<u8>,
}

impl App {
    /// Parse and validate the import buffer as a `SigningResponse`, then add
    /// the signature to the collected set.
    pub(crate) fn import_cosigner_response(&mut self) {
        tracing::info!("Import cosigner response triggered.");

        let (request_msg, n_signers, signatures, import_response_json) = match &mut self.tx_status {
            TransactionStatus::AwaitingCoSigners {
                request,
                signatures,
                import_response_json,
                ..
            } => (
                request.signing_message.clone(),
                request.multisig_config.signers().len(),
                signatures,
                import_response_json,
            ),
            _ => {
                tracing::error!("import_cosigner_response: not in AwaitingCoSigners state.");
                return;
            }
        };

        let buf = import_response_json.trim().to_string();
        tracing::info!("Import buffer length: {} chars.", buf.len());
        if buf.is_empty() {
            self.status = crate::types::Status::Error("Paste a response first.".to_string());
            return;
        }

        let response: qpv2_core::types::SigningResponse = match serde_json::from_str(&buf) {
            Ok(r) => r,
            Err(e) => {
                self.status = crate::types::Status::Error(format!("Invalid response JSON: {}", e));
                return;
            }
        };

        if response.signing_message != request_msg {
            self.status =
                crate::types::Status::Error("Response signing message does not match.".to_string());
            return;
        }

        if response.signer_index >= n_signers {
            self.status = crate::types::Status::Error(format!(
                "Signer index {} out of range (N={}).",
                response.signer_index, n_signers
            ));
            return;
        }

        if signatures
            .iter()
            .any(|(idx, _)| *idx == response.signer_index)
        {
            self.status = crate::types::Status::Error(format!(
                "Signer {} already provided a signature.",
                response.signer_index
            ));
            return;
        }

        let sig_bytes = match hex::decode(&response.signature) {
            Ok(b) => b,
            Err(e) => {
                self.status = crate::types::Status::Error(format!("Invalid signature hex: {}", e));
                return;
            }
        };

        signatures.push((response.signer_index, sig_bytes));
        import_response_json.clear();
        self.status = crate::types::Status::Info(format!(
            "Signature from signer {} imported.",
            response.signer_index
        ));
    }

    /// Checks a co-signer's request against THIS wallet's accounts: which
    /// account signs, at which position among the signers, and the message.
    fn find_local_signer(&self, request: &SigningRequest) -> Result<CosignSigner, String> {
        let variant = KeyVault::get_spx_variant(self.wallet_id)
            .map_err(|e| format!("Failed to read variant: {}", e))?;
        let singlesig_accounts = KeyVault::get_singlesig_accounts(self.wallet_id)
            .map_err(|e| format!("Failed to load accounts: {}", e))?;

        let mut matched = None;
        for account in &singlesig_accounts {
            let account_pubkey = account.config.signers()[0].pubkey();
            for (i, signer) in request.multisig_config.signers().iter().enumerate() {
                if signer.pubkey() == account_pubkey && signer.variant() == variant {
                    matched = Some((i, account.lock_args.clone()));
                    break;
                }
            }
            if matched.is_some() {
                break;
            }
        }
        let (signer_index, lock_args) = matched
            .ok_or_else(|| "No local account matches a signer in this request.".to_string())?;

        let message = hex::decode(&request.signing_message)
            .map_err(|e| format!("Invalid signing message hex: {}", e))?;

        Ok(CosignSigner {
            variant,
            signer_index,
            lock_args,
            message,
        })
    }

    /// Co-signs on a password or Touch ID wallet: checks the request, asks
    /// for the key now, signs, then shows the response.
    pub(crate) fn co_sign(&mut self, request: SigningRequest) {
        let signer = match self.find_local_signer(&request) {
            Ok(s) => s,
            Err(e) => {
                self.status = Status::Error(e);
                return;
            }
        };

        // Authenticate. Trezor is refused up front — the firmware only signs
        // a transaction locked by its own single-sig lock, so there is no
        // device flow for `resolve_auth_key`'s generic Trezor message to
        // point at.
        if matches!(self.auth_method, Some(AuthMethod::Trezor { .. })) {
            self.status = Status::Error(
                "Co-signing a multisig transaction on a Trezor is not supported yet.".to_string(),
            );
            return;
        }
        let auth = match self.resolve_auth_key("co-sign a multisig transaction") {
            Ok(a) => a,
            Err(e) => {
                self.status = Status::Error(format!("Auth failed: {}", e));
                return;
            }
        };

        match sign(self.wallet_id, auth, &request, signer) {
            Ok(response) => self.show_cosign_response(response),
            Err(e) => self.status = Status::Error(e),
        }
    }

    /// Co-signs on a FIDO2 wallet: checks the request, then the security key
    /// and the signing run in the background, so the window stays live while
    /// the key waits for a touch; `poll_fido2_cosign` shows the response.
    pub(crate) fn co_sign_with_fido2(&mut self, request: SigningRequest) {
        let signer = match self.find_local_signer(&request) {
            Ok(s) => s,
            Err(e) => {
                self.status = Status::Error(e);
                return;
            }
        };
        let credential_id = match self.get_fido2_credential() {
            Ok(c) => c,
            Err(e) => {
                self.status = Status::Error(e);
                return;
            }
        };
        let wallet_id = self.wallet_id;
        let job = move || {
            let auth = get_fido2_dek(&credential_id, "co-sign a multisig transaction")
                .map_err(|e| format!("Auth failed: {}", e))?;
            sign(wallet_id, auth, &request, signer)
        };
        match spawn_fido2_job(job) {
            Ok(rx) => self.fido2_cosign_rx = Some(rx),
            Err(e) => self.status = Status::Error(e),
        }
    }

    /// Puts a signed co-signer response on screen, to copy back.
    pub(crate) fn show_cosign_response(&mut self, response: String) {
        self.cosign_response_json = Some(response);
        self.status = Status::Info("Signed! Copy the response below.".to_string());
    }
}

/// Signs a co-signer's request with the wallet key `auth` and returns the
/// response to send back. Touches no UI state, so it can run on a worker.
fn sign(
    wallet_id: u32,
    auth: AuthKey,
    request: &SigningRequest,
    signer: CosignSigner,
) -> Result<String, String> {
    let vault = KeyVault::new(signer.variant, wallet_id);
    let (signature, _pubkey) = vault
        .raw_sign(auth, signer.lock_args, signer.message)
        .map_err(|e| format!("Signing failed: {}", e))?;
    let response = SigningResponse {
        version: request.version,
        signer_index: signer.signer_index,
        signature: hex::encode(&signature),
        signing_message: request.signing_message.clone(),
    };
    serde_json::to_string_pretty(&response)
        .map_err(|e| format!("Failed to serialize response: {}", e))
}
