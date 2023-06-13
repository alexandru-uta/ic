use crate::{
    logs::{ERROR, INFO},
    pb::v1::{
        error_refund_icp_response, set_dapp_controllers_call_result, set_mode_call_result,
        set_mode_call_result::SetModeResult,
        settle_community_fund_participation_result,
        sns_neuron_recipe::{ClaimedStatus, Investor},
        BuyerState, CfInvestment, CfNeuron, CfParticipant, DirectInvestment,
        ErrorRefundIcpResponse, FinalizeSwapResponse, Init, Lifecycle, NeuronId as SaleNeuronId,
        OpenRequest, Params, SetDappControllersCallResult, SetModeCallResult,
        SettleCommunityFundParticipationResult, SnsNeuronRecipe, SweepResult, TransferableAmount,
    },
    swap::is_valid_principal,
};
use ic_base_types::{CanisterId, PrincipalId};
use ic_canister_log::log;
use ic_ledger_core::Tokens;
use ic_nervous_system_common::{ledger::ICRC1Ledger, SECONDS_PER_DAY};
use ic_sns_governance::pb::v1::{ClaimedSwapNeuronStatus, NeuronId};
use icrc_ledger_types::icrc1::account::{Account, Subaccount};
use std::str::FromStr;

pub fn validate_principal(p: &str) -> Result<(), String> {
    let _ = PrincipalId::from_str(p).map_err(|x| {
        format!(
            "Couldn't validate PrincipalId. String \"{}\" could not be converted to PrincipalId: {}",
            p, x
        )
    })?;
    Ok(())
}

pub fn validate_canister_id(p: &str) -> Result<(), String> {
    let pp = PrincipalId::from_str(p).map_err(|x| {
        format!(
            "Couldn't validate CanisterId. String \"{}\" could not be converted to PrincipalId: {}",
            p, x
        )
    })?;
    let _cid = CanisterId::new(pp).map_err(|x| {
        format!(
            "Couldn't validate CanisterId. PrincipalId \"{}\" could not be converted to CanisterId: {}",
            pp,
            x
        )
    })?;
    Ok(())
}

impl ErrorRefundIcpResponse {
    pub(crate) fn new_ok(block_height: u64) -> Self {
        use error_refund_icp_response::{Ok, Result};

        Self {
            result: Some(Result::Ok(Ok {
                block_height: Some(block_height),
            })),
        }
    }

    pub(crate) fn new_precondition_error(description: impl ToString) -> Self {
        Self::new_error(
            error_refund_icp_response::err::Type::Precondition,
            description,
        )
    }

    pub(crate) fn new_invalid_request_error(description: impl ToString) -> Self {
        Self::new_error(
            error_refund_icp_response::err::Type::InvalidRequest,
            description,
        )
    }

    pub(crate) fn new_external_error(description: impl ToString) -> Self {
        Self::new_error(error_refund_icp_response::err::Type::External, description)
    }

    fn new_error(
        error_type: error_refund_icp_response::err::Type,
        description: impl ToString,
    ) -> Self {
        use error_refund_icp_response::{Err, Result};

        Self {
            result: Some(Result::Err(Err {
                error_type: Some(error_type as i32),
                description: Some(description.to_string()),
            })),
        }
    }
}

impl Init {
    pub fn nns_governance_or_panic(&self) -> CanisterId {
        CanisterId::new(PrincipalId::from_str(&self.nns_governance_canister_id).unwrap()).unwrap()
    }

    pub fn nns_governance(&self) -> Result<CanisterId, String> {
        let principal_id = PrincipalId::from_str(&self.nns_governance_canister_id)
            .map_err(|err| err.to_string())?;

        CanisterId::new(principal_id).map_err(|err| err.to_string())
    }

    pub fn sns_root_or_panic(&self) -> CanisterId {
        CanisterId::new(PrincipalId::from_str(&self.sns_root_canister_id).unwrap()).unwrap()
    }

    pub fn sns_governance_or_panic(&self) -> CanisterId {
        CanisterId::new(PrincipalId::from_str(&self.sns_governance_canister_id).unwrap()).unwrap()
    }

    pub fn sns_governance(&self) -> Result<CanisterId, String> {
        let principal_id = PrincipalId::from_str(&self.sns_governance_canister_id)
            .map_err(|err| err.to_string())?;

        CanisterId::new(principal_id).map_err(|err| err.to_string())
    }

    pub fn sns_ledger_or_panic(&self) -> CanisterId {
        CanisterId::new(PrincipalId::from_str(&self.sns_ledger_canister_id).unwrap()).unwrap()
    }

    pub fn icp_ledger_or_panic(&self) -> CanisterId {
        CanisterId::new(PrincipalId::from_str(&self.icp_ledger_canister_id).unwrap()).unwrap()
    }

    pub fn transaction_fee_e8s_or_panic(&self) -> u64 {
        self.transaction_fee_e8s.unwrap()
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_canister_id(&self.nns_governance_canister_id)?;
        validate_canister_id(&self.sns_governance_canister_id)?;
        validate_canister_id(&self.sns_ledger_canister_id)?;
        validate_canister_id(&self.icp_ledger_canister_id)?;
        validate_canister_id(&self.sns_root_canister_id)?;

        if self.fallback_controller_principal_ids.is_empty() {
            return Err("at least one fallback controller required".to_string());
        }
        for fc in &self.fallback_controller_principal_ids {
            validate_principal(fc)?;
        }

        if self.transaction_fee_e8s.is_none() {
            return Err("transaction_fee_e8s is required.".to_string());
        }
        // The value itself is not checked; only that it is supplied. Needs to
        // match the value in SNS ledger though.

        if self.neuron_minimum_stake_e8s.is_none() {
            return Err("neuron_minimum_stake_e8s is required.".to_string());
        }
        // As with transaction_fee_e8s, the value itself is not checked; only
        // that it is supplied. Needs to match the value in SNS governance
        // though.

        Ok(())
    }
}

impl Params {
    const MIN_SALE_DURATION_SECONDS: u64 = SECONDS_PER_DAY;
    const MAX_SALE_DURATION_SECONDS: u64 = 90 * SECONDS_PER_DAY;

    pub fn validate(&self, init: &Init) -> Result<(), String> {
        if self.min_icp_e8s == 0 {
            return Err("min_icp_e8s must be > 0".to_string());
        }

        if self.min_participants == 0 {
            return Err("min_participants must be > 0".to_string());
        }

        let transaction_fee_e8s = init
            .transaction_fee_e8s
            .expect("transaction_fee_e8s was not supplied.");

        let neuron_minimum_stake_e8s = init
            .neuron_minimum_stake_e8s
            .expect("neuron_minimum_stake_e8s was not supplied");

        let neuron_basket_count = self
            .neuron_basket_construction_parameters
            .as_ref()
            .expect("participant_neuron_basket not populated.")
            .count as u128;

        let min_participant_sns_e8s = self.min_participant_icp_e8s as u128
            * self.sns_token_e8s as u128
            / self.max_icp_e8s as u128;

        let min_participant_icp_e8s_big_enough = min_participant_sns_e8s
            >= neuron_basket_count * (neuron_minimum_stake_e8s + transaction_fee_e8s) as u128;

        if !min_participant_icp_e8s_big_enough {
            return Err(format!(
                "min_participant_icp_e8s={} is too small. It needs to be \
                 large enough to ensure that participants will end up with \
                 enough SNS tokens to form {} SNS neurons, each of which \
                 require at least {} SNS e8s, plus {} e8s in transaction \
                 fees. More precisely, the following inequality must hold: \
                 min_participant_icp_e8s >= neuron_basket_count * (neuron_minimum_stake_e8s + transaction_fee_e8s) * max_icp_e8s / sns_token_e8s \
                 (where / denotes floor division).",
                self.min_participant_icp_e8s,
                neuron_basket_count,
                neuron_minimum_stake_e8s,
                transaction_fee_e8s,
            ));
        }

        if self.sns_token_e8s == 0 {
            return Err("sns_token_e8s must be > 0".to_string());
        }

        if self.max_participant_icp_e8s < self.min_participant_icp_e8s {
            return Err(format!(
                "max_participant_icp_e8s ({}) must be >= min_participant_icp_e8s ({})",
                self.max_participant_icp_e8s, self.min_participant_icp_e8s
            ));
        }

        if self.min_icp_e8s > self.max_icp_e8s {
            return Err(format!(
                "min_icp_e8s ({}) must be <= max_icp_e8s ({})",
                self.min_icp_e8s, self.max_icp_e8s
            ));
        }

        if self.max_participant_icp_e8s > self.max_icp_e8s {
            return Err(format!(
                "max_participant_icp_e8s ({}) must be <= max_icp_e8s ({})",
                self.max_participant_icp_e8s, self.max_icp_e8s
            ));
        }

        // Cap `max_icp_e8s` at 1 billion ICP
        if self.max_icp_e8s > /* 1B */ 1_000_000_000 * /* e8s per ICP */ 100_000_000 {
            return Err(format!(
                "max_icp_e8s ({}) can be at most 1B ICP",
                self.max_icp_e8s
            ));
        }

        // 100 * 1B * E8S should fit in a u64.
        assert!(self
            .max_icp_e8s
            .checked_mul(self.min_participants as u64)
            .is_some());

        if self.max_icp_e8s
            < (self.min_participants as u64).saturating_mul(self.min_participant_icp_e8s)
        {
            return Err(format!(
                "max_icp_e8s ({}) must be >= min_participants ({}) * min_participant_icp_e8s ({})",
                self.max_icp_e8s, self.min_participants, self.min_participant_icp_e8s
            ));
        }

        if self.neuron_basket_construction_parameters.is_none() {
            return Err("neuron_basket_construction_parameters must be provided".to_string());
        }

        let neuron_basket = self
            .neuron_basket_construction_parameters
            .as_ref()
            .expect("Expected neuron_basket_construction_parameters to be set");

        if neuron_basket.count == 0 {
            return Err(format!(
                "neuron_basket_construction_parameters.count ({}) must be > 0",
                neuron_basket.count,
            ));
        }

        if neuron_basket.dissolve_delay_interval_seconds == 0 {
            return Err(format!(
                "neuron_basket_construction_parameters.dissolve_delay_interval_seconds ({}) must be > 0",
                neuron_basket.dissolve_delay_interval_seconds,
            ));
        }

        let maximum_dissolve_delay = neuron_basket
            .count
            .saturating_mul(neuron_basket.dissolve_delay_interval_seconds)
            .saturating_add(1);

        if maximum_dissolve_delay == u64::MAX {
            return Err(
                "Chosen neuron_basket_construction_parameters will result in u64 overflow"
                    .to_string(),
            );
        }

        Ok(())
    }

    pub fn is_valid_if_initiated_at(&self, now_seconds: u64) -> bool {
        let sale_delay_seconds = self.sale_delay_seconds.unwrap_or(0);

        let open_timestamp_seconds = now_seconds.saturating_add(sale_delay_seconds);
        let duration_seconds = self
            .swap_due_timestamp_seconds
            .saturating_sub(open_timestamp_seconds);

        // Sale must be at least MIN_SALE_DURATION_SECONDS long
        if duration_seconds < Self::MIN_SALE_DURATION_SECONDS {
            return false;
        }
        // Sale can be at most MAX_SALE_DURATION_SECONDS long
        if duration_seconds > Self::MAX_SALE_DURATION_SECONDS {
            return false;
        }

        true
    }
}

impl BuyerState {
    pub fn new(amount_icp_e8s: u64) -> Self {
        Self {
            icp: Some(TransferableAmount {
                amount_e8s: amount_icp_e8s,
                transfer_start_timestamp_seconds: 0,
                transfer_success_timestamp_seconds: 0,
                amount_transferred_e8s: Some(0),
                transfer_fee_paid_e8s: Some(0),
            }),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if let Some(icp) = &self.icp {
            icp.validate()
        } else {
            Err("Field 'icp' is missing but required".to_string())
        }
    }

    pub fn amount_icp_e8s(&self) -> u64 {
        if let Some(icp) = &self.icp {
            return icp.amount_e8s;
        }
        0
    }

    pub fn set_amount_icp_e8s(&mut self, val: u64) {
        if let Some(ref mut icp) = &mut self.icp {
            icp.amount_e8s = val;
        } else {
            self.icp = Some(TransferableAmount {
                amount_e8s: val,
                transfer_start_timestamp_seconds: 0,
                transfer_success_timestamp_seconds: 0,
                amount_transferred_e8s: Some(0),
                transfer_fee_paid_e8s: Some(0),
            });
        }
    }
}

impl TransferableAmount {
    pub fn validate(&self) -> Result<(), String> {
        if self.transfer_start_timestamp_seconds == 0 && self.transfer_success_timestamp_seconds > 0
        {
            // Successful transfer without start time.
            return Err(format!("Invariant violation: transfer_start_timestamp_seconds is zero but transfer_success_timestamp_seconds ({}) is non-zero", self.transfer_success_timestamp_seconds));
        }
        if self.transfer_start_timestamp_seconds > self.transfer_success_timestamp_seconds
            && self.transfer_success_timestamp_seconds > 0
        {
            // Successful transfer before the transfer started.
            return Err(format!("Invariant violation: transfer_start_timestamp_seconds ({}) > transfer_success_timestamp_seconds ({}) > 0", self.transfer_start_timestamp_seconds, self.transfer_success_timestamp_seconds));
        }
        Ok(())
    }

    pub(crate) async fn transfer_helper(
        &mut self,
        now_fn: fn(bool) -> u64,
        fee: Tokens,
        subaccount: Option<Subaccount>,
        dst: &Account,
        ledger: &dyn ICRC1Ledger,
    ) -> TransferResult {
        let amount = Tokens::from_e8s(self.amount_e8s);
        if amount <= fee {
            // Skip: amount too small...
            return TransferResult::AmountTooSmall;
        }
        if self.transfer_start_timestamp_seconds > 0 {
            // Operation in progress...
            return TransferResult::AlreadyStarted;
        }
        self.transfer_start_timestamp_seconds = now_fn(false);

        // The ICRC1Ledger Trait converts any errors to Err(NervousSystemError).
        // No panics should occur when issuing this transfer.
        let result = ledger
            .transfer_funds(
                amount.get_e8s().saturating_sub(fee.get_e8s()),
                fee.get_e8s(),
                subaccount,
                *dst,
                0,
            )
            .await;
        if self.transfer_start_timestamp_seconds == 0 {
            log!(
                ERROR,
                "Token disburse logic error: expected transfer start time",
            );
        }
        match result {
            Ok(h) => {
                self.transfer_success_timestamp_seconds = now_fn(true);
                log!(
                    INFO,
                    "Transferred {} from subaccount {:?} to {} at height {} in Ledger Canister {}",
                    amount,
                    subaccount,
                    dst,
                    h,
                    ledger.canister_id()
                );
                TransferResult::Success(h)
            }
            Err(e) => {
                self.transfer_start_timestamp_seconds = 0;
                self.transfer_success_timestamp_seconds = 0;
                log!(
                    ERROR,
                    "Failed to transfer {} from subaccount {:#?}: {}",
                    amount,
                    subaccount,
                    e
                );
                TransferResult::Failure(e.to_string())
            }
        }
    }
}

impl OpenRequest {
    pub fn validate(&self, current_timestamp_seconds: u64, init: &Init) -> Result<(), String> {
        let mut defects = vec![];

        // Inspect params.
        match self.params.as_ref() {
            None => {
                defects.push("The parameters of the swap are missing.".to_string());
            }
            Some(params) => {
                if !params.is_valid_if_initiated_at(current_timestamp_seconds) {
                    defects.push("The parameters of the swap are invalid.".to_string());
                } else if let Err(err) = params.validate(init) {
                    defects.push(err);
                }
            }
        }

        // Inspect open_sns_token_swap_proposal_id.
        if self.open_sns_token_swap_proposal_id.is_none() {
            defects.push("The open_sns_token_swap_proposal_id field has no value.".to_string());
        }

        // Return result.
        if defects.is_empty() {
            Ok(())
        } else {
            Err(defects.join("\n"))
        }
    }
}

impl DirectInvestment {
    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_principal(&self.buyer_principal) {
            return Err(format!("Invalid principal {}", self.buyer_principal));
        }
        Ok(())
    }
}

impl CfInvestment {
    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_principal(&self.hotkey_principal) {
            return Err(format!(
                "Invalid hotkey principal {}",
                self.hotkey_principal
            ));
        }
        if self.nns_neuron_id == 0 {
            return Err("Missing nns_neuron_id".to_string());
        }
        Ok(())
    }
}

impl SnsNeuronRecipe {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(sns) = &self.sns {
            sns.validate()?;
        } else {
            return Err("Missing required field 'sns'".to_string());
        }
        match &self.investor {
            Some(Investor::Direct(di)) => di.validate()?,
            Some(Investor::CommunityFund(cf)) => cf.validate()?,
            None => return Err("Missing required field 'investor'".to_string()),
        }
        Ok(())
    }
}

impl CfParticipant {
    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_principal(&self.hotkey_principal) {
            return Err(format!(
                "Invalid hotkey principal {}",
                self.hotkey_principal
            ));
        }
        if self.cf_neurons.is_empty() {
            return Err(format!(
                "A CF participant ({}) must have at least one neuron",
                self.hotkey_principal
            ));
        }
        for n in &self.cf_neurons {
            n.validate()?;
        }
        Ok(())
    }
    pub fn participant_total_icp_e8s(&self) -> u64 {
        self.cf_neurons
            .iter()
            .map(|x| x.amount_icp_e8s)
            .fold(0, |sum, v| sum.saturating_add(v))
    }
}

impl CfNeuron {
    pub fn validate(&self) -> Result<(), String> {
        if self.nns_neuron_id == 0 {
            return Err("nns_neuron_id must be specified".to_string());
        }
        if self.amount_icp_e8s == 0 {
            return Err("amount_icp_e8s must be specified".to_string());
        }
        Ok(())
    }
}

impl Lifecycle {
    pub fn is_terminal(&self) -> bool {
        match self {
            Self::Committed | Self::Aborted => true,

            Self::Pending | Self::Adopted | Self::Open => false,
            Self::Unspecified => {
                log!(ERROR, "A wild Lifecycle::Unspecified appeared.",);
                false
            }
        }
    }
}

/// Result of a token transfer (commit or abort) on a ledger (ICP or
/// SNS) for a single buyer.
pub enum TransferResult {
    /// Transfer was skipped as the amount was less than the requested fee.
    AmountTooSmall,
    /// Transferred was skipped as an operation is already in progress or completed.
    AlreadyStarted,
    /// The operation was successful at the specified block height.
    Success(u64),
    /// The operation failed with the specified error message.
    Failure(String),
}

impl TransferResult {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success(_))
    }
}

/// Intermediate struct used when generating the basket of neurons for investors.
#[derive(PartialEq, Eq, Debug)]
pub(crate) struct ScheduledVestingEvent {
    /// The dissolve_delay of the neuron
    pub(crate) dissolve_delay_seconds: u64,
    /// The amount of tokens in e8s
    pub(crate) amount_e8s: u64,
}

impl FinalizeSwapResponse {
    pub fn with_error(error_message: String) -> Self {
        FinalizeSwapResponse {
            error_message: Some(error_message),
            ..Default::default()
        }
    }

    pub fn set_error_message(&mut self, error_message: String) {
        self.error_message = Some(error_message)
    }

    pub fn set_sweep_icp_result(&mut self, sweep_icp_result: SweepResult) {
        if !sweep_icp_result.is_successful_sweep() {
            self.set_error_message(
                "Transferring ICP did not complete fully, some transfers were invalid or failed. Halting sale finalization".to_string()
            );
        }
        self.sweep_icp_result = Some(sweep_icp_result);
    }

    pub fn set_settle_community_fund_participation_result(
        &mut self,
        result: SettleCommunityFundParticipationResult,
    ) {
        if !result.is_successful_settlement() {
            self.set_error_message(
                "Settling the CommunityFund participation did not succeed. Halting sale finalization".to_string());
        }
        self.settle_community_fund_participation_result = Some(result);
    }

    pub fn set_set_dapp_controllers_result(&mut self, result: SetDappControllersCallResult) {
        if !result.is_successful_set_dapp_controllers() {
            self.set_error_message(
                "Restoring the dapp canisters controllers did not succeed. Halting sale finalization".to_string());
        }
        self.set_dapp_controllers_call_result = Some(result);
    }

    pub fn set_sweep_sns_result(&mut self, sweep_sns_result: SweepResult) {
        if !sweep_sns_result.is_successful_sweep() {
            self.set_error_message(
                "Transferring SNS tokens did not complete fully, some transfers were invalid or failed. Halting sale finalization".to_string()
            );
        }
        self.sweep_sns_result = Some(sweep_sns_result);
    }

    pub fn set_claim_neuron_result(&mut self, claim_neuron_result: SweepResult) {
        if !claim_neuron_result.is_successful_sweep() {
            self.set_error_message(
                "Claiming SNS Neurons did not complete fully, some claims were invalid or failed. Halting sale finalization".to_string()
            );
        }
        self.claim_neuron_result = Some(claim_neuron_result);
    }

    pub fn set_set_mode_call_result(&mut self, set_mode_call_result: SetModeCallResult) {
        if !set_mode_call_result.is_successful_set_mode_call() {
            self.set_error_message(
                "Setting the SNS Governance mode to normal did not complete fully. Halting sale finalization".to_string()
            );
        }
        self.set_mode_call_result = Some(set_mode_call_result);
    }

    pub fn has_error_message(&self) -> bool {
        self.error_message.is_some()
    }
}

impl SweepResult {
    fn is_successful_sweep(&self) -> bool {
        let SweepResult {
            failure,
            invalid,
            success: _,
            skipped: _,
            global_failures,
        } = self;
        *failure == 0 && *invalid == 0 && *global_failures == 0
    }

    pub(crate) fn new_with_global_failures(global_failures: u32) -> Self {
        SweepResult {
            global_failures,
            ..Default::default()
        }
    }

    pub(crate) fn consume(&mut self, consumable: SweepResult) {
        let SweepResult {
            failure,
            invalid,
            success,
            skipped,
            global_failures,
        } = consumable;

        self.failure += failure;
        self.invalid += invalid;
        self.success += success;
        self.skipped += skipped;
        self.global_failures += global_failures;
    }
}

impl SettleCommunityFundParticipationResult {
    fn is_successful_settlement(&self) -> bool {
        use settle_community_fund_participation_result::Response;
        matches!(
            &self.possibility,
            Some(settle_community_fund_participation_result::Possibility::Ok(
                Response {
                    governance_error: None,
                }
            ))
        )
    }
}

impl SetDappControllersCallResult {
    fn is_successful_set_dapp_controllers(&self) -> bool {
        match &self.possibility {
            Some(set_dapp_controllers_call_result::Possibility::Ok(response)) => {
                response.failed_updates.is_empty()
            }
            _ => false,
        }
    }
}

impl SetModeCallResult {
    pub fn is_successful_set_mode_call(&self) -> bool {
        matches!(
            &self.possibility,
            Some(set_mode_call_result::Possibility::Ok(SetModeResult {}))
        )
    }
}

/// The mapping of ClaimedSwapNeuronStatus to ClaimedStatus
impl From<ClaimedSwapNeuronStatus> for ClaimedStatus {
    fn from(claimed_swap_neuron_status: ClaimedSwapNeuronStatus) -> Self {
        match claimed_swap_neuron_status {
            ClaimedSwapNeuronStatus::Success => ClaimedStatus::Success,
            ClaimedSwapNeuronStatus::Unspecified => ClaimedStatus::Failed,
            ClaimedSwapNeuronStatus::MemoryExhausted => ClaimedStatus::Failed,
            ClaimedSwapNeuronStatus::Invalid => ClaimedStatus::Invalid,
            ClaimedSwapNeuronStatus::AlreadyExists => ClaimedStatus::Invalid,
        }
    }
}

// TODO NNS1-1589: Implementation will not longer be needed when swap.proto can depend on
// SNS governance.proto
impl From<[u8; 32]> for SaleNeuronId {
    fn from(value: [u8; 32]) -> Self {
        Self { id: value.to_vec() }
    }
}

// TODO NNS1-1589: Implementation will not longer be needed when swap.proto can depend on
// SNS governance.proto
impl From<NeuronId> for SaleNeuronId {
    fn from(neuron_id: NeuronId) -> Self {
        Self { id: neuron_id.id }
    }
}

// TODO NNS1-1589: Implementation will not longer be needed when swap.proto can depend on
// SNS governance.proto
impl TryInto<NeuronId> for SaleNeuronId {
    type Error = String;

    fn try_into(self) -> Result<NeuronId, Self::Error> {
        match Subaccount::try_from(self.id) {
            Ok(subaccount) => Ok(NeuronId::from(subaccount)),
            Err(err) => Err(format!(
                "Followee could not be parsed into NeuronId. Err {:?}",
                err
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        pb::v1::{
            params::NeuronBasketConstructionParameters, CfNeuron, CfParticipant, Init,
            ListDirectParticipantsResponse, OpenRequest, Params, Participant,
        },
        swap::MAX_LIST_DIRECT_PARTICIPANTS_LIMIT,
    };
    use ic_base_types::PrincipalId;
    use ic_nervous_system_common::{
        assert_is_err, assert_is_ok, E8, SECONDS_PER_DAY, START_OF_2022_TIMESTAMP_SECONDS,
    };
    use lazy_static::lazy_static;
    use prost::Message;
    use std::mem;

    const OPEN_SNS_TOKEN_SWAP_PROPOSAL_ID: u64 = 489102;

    const PARAMS: Params = Params {
        max_icp_e8s: 1_000 * E8,
        max_participant_icp_e8s: 1_000 * E8,
        min_icp_e8s: 10 * E8,
        min_participant_icp_e8s: 5 * E8,
        sns_token_e8s: 5_000 * E8,
        min_participants: 10,
        swap_due_timestamp_seconds: START_OF_2022_TIMESTAMP_SECONDS + 14 * SECONDS_PER_DAY,
        neuron_basket_construction_parameters: Some(NeuronBasketConstructionParameters {
            count: 3,
            dissolve_delay_interval_seconds: 7890000, // 3 months
        }),
        sale_delay_seconds: None,
    };

    lazy_static! {
        static ref OPEN_REQUEST: OpenRequest = OpenRequest {
            params: Some(PARAMS),
            cf_participants: vec![CfParticipant {
                hotkey_principal: PrincipalId::new_user_test_id(423939).to_string(),
                cf_neurons: vec![CfNeuron {
                    nns_neuron_id: 42,
                    amount_icp_e8s: 99,
                }],
            },],
            open_sns_token_swap_proposal_id: Some(OPEN_SNS_TOKEN_SWAP_PROPOSAL_ID),
        };

        // Fill out Init just enough to test Params validation. These values are
        // similar to, but not the same analogous values in NNS.
        static ref INIT: Init = Init {
            transaction_fee_e8s: Some(12_345),
            neuron_minimum_stake_e8s: Some(123_456_789),
            ..Default::default()
        };
    }

    #[test]
    fn accept_iff_can_form_sns_neuron_in_the_worst_case() {
        let mut init = INIT.clone();

        let sns_token_e8s = PARAMS.min_participant_icp_e8s as u128 * PARAMS.sns_token_e8s as u128
            / PARAMS.max_icp_e8s as u128;
        let neuron_basket_count = PARAMS
            .neuron_basket_construction_parameters
            .as_ref()
            .expect("participant_neuron_basket not populated.")
            .count as u128;
        let available_sns_token_e8s_per_neuron =
            sns_token_e8s / neuron_basket_count - init.transaction_fee_e8s.unwrap() as u128;
        assert!(available_sns_token_e8s_per_neuron < u64::MAX as u128);
        let available_sns_token_e8s_per_neuron = available_sns_token_e8s_per_neuron as u64;
        assert!(init.neuron_minimum_stake_e8s.unwrap() <= available_sns_token_e8s_per_neuron);

        // Set the bar as high as min_participant_icp_e8s can "jump".
        init.neuron_minimum_stake_e8s = Some(available_sns_token_e8s_per_neuron);
        assert_is_ok!(PARAMS.validate(&init));

        // The bar can still be cleared if lowered.
        init.neuron_minimum_stake_e8s = Some(available_sns_token_e8s_per_neuron - 1);
        assert_is_ok!(PARAMS.validate(&init));

        // Raise the bar so that it can no longer be cleared.
        init.neuron_minimum_stake_e8s = Some(available_sns_token_e8s_per_neuron + 1);
        assert_is_err!(PARAMS.validate(&init));
    }

    #[test]
    fn open_request_validate_ok() {
        assert_is_ok!(OPEN_REQUEST.validate(START_OF_2022_TIMESTAMP_SECONDS, &INIT));
    }

    #[test]
    fn params_high_participants_validate_ok() {
        let params = Params {
            min_participants: 500,
            // max_icp_e8s must be enough for all of min_participants to participate
            max_icp_e8s: 500 * PARAMS.min_participant_icp_e8s,
            ..PARAMS
        };
        params.validate(&INIT).unwrap();
    }

    #[test]
    fn open_request_validate_invalid_params() {
        let request = OpenRequest {
            params: Some(Params {
                swap_due_timestamp_seconds: 42,
                ..PARAMS.clone()
            }),
            ..OPEN_REQUEST.clone()
        };

        assert_is_err!(request.validate(START_OF_2022_TIMESTAMP_SECONDS, &INIT));
    }

    #[test]
    fn open_request_validate_no_proposal_id() {
        let request = OpenRequest {
            open_sns_token_swap_proposal_id: None,
            ..OPEN_REQUEST.clone()
        };

        assert_is_err!(request.validate(START_OF_2022_TIMESTAMP_SECONDS, &INIT));
    }

    #[test]
    fn participant_total_icp_e8s_no_overflow() {
        let participant = CfParticipant {
            hotkey_principal: "".to_string(),
            cf_neurons: vec![
                CfNeuron {
                    nns_neuron_id: 0,
                    amount_icp_e8s: u64::MAX,
                },
                CfNeuron {
                    nns_neuron_id: 0,
                    amount_icp_e8s: u64::MAX,
                },
            ],
        };
        let total = participant.participant_total_icp_e8s();
        assert_eq!(total, u64::MAX);
    }

    #[test]
    fn large_community_fund_does_not_result_in_over_sized_open_request() {
        const MAX_SIZE_BYTES: usize = 1 << 21; // 2 Mi

        let neurons_per_principal = 3;

        let cf_participant = CfParticipant {
            hotkey_principal: PrincipalId::new_user_test_id(789362).to_string(),
            cf_neurons: (0..neurons_per_principal)
                .map(|_| CfNeuron {
                    nns_neuron_id: 592523,
                    amount_icp_e8s: 1_000 * E8,
                })
                .collect(),
        };

        let mut open_request = OpenRequest {
            cf_participants: vec![cf_participant],
            ..Default::default()
        };

        // Crescendo
        loop {
            let mut buffer: Vec<u8> = vec![];
            open_request.encode(&mut buffer).unwrap();
            if buffer.len() > MAX_SIZE_BYTES {
                break;
            }

            // Double size of cf_participants.
            open_request
                .cf_participants
                .append(&mut open_request.cf_participants.clone());
        }

        // TODO: Get more precise using our favorite algo: binary search!
        let safe_len = open_request.cf_participants.len() / 2;
        assert!(safe_len > 10_000);
        println!(
            "Looks like we can support at least {} Community Fund neurons (among {} principals).",
            safe_len * neurons_per_principal,
            safe_len,
        );
    }

    /// Test that the configured MAX_LIST_DIRECT_PARTICIPANTS_LIMIT will efficiently pack
    /// Participants and not exceed the message size limits of the IC.
    #[test]
    fn test_list_direct_participation_limit_is_accurate_and_efficient() {
        let max_inter_canister_payload_in_bytes = 2 * 1024 * 1024; // 2 MiB
        let participant_size = mem::size_of::<Participant>();
        let response_size = mem::size_of::<ListDirectParticipantsResponse>();

        // Account for Response overhead, then divide the max message size by the memory footprint
        // of the participant.
        let participants_per_message =
            (max_inter_canister_payload_in_bytes - response_size) / participant_size;

        assert!(
            participants_per_message >= MAX_LIST_DIRECT_PARTICIPANTS_LIMIT as usize,
            "The currently compiled MAX_LIST_DIRECT_PARTICIPANTS_LIMIT is greater than what can \
            fit in a single inter canister message. Calculated participants per message: {}. \
            Configured limit: {}",
            participants_per_message,
            MAX_LIST_DIRECT_PARTICIPANTS_LIMIT
        );

        let remainder = participants_per_message - MAX_LIST_DIRECT_PARTICIPANTS_LIMIT as usize;
        assert!(
            remainder < 5000,
            "An increment of more than 5000 participants ({}) can be added to the \
            ListDirectParticipantsResponse without reaching the max message size. Update \
            MAX_LIST_DIRECT_PARTICIPANTS_LIMIT and the corresponding API docs",
            remainder
        );
    }

    #[test]
    fn sale_cannot_be_open_more_than_90_days() {
        // Should be valid with the sale deadline set to MAX_SALE_DURATION_SECONDS from now.
        let params = Params {
            swap_due_timestamp_seconds: Params::MAX_SALE_DURATION_SECONDS,
            sale_delay_seconds: Some(0),
            ..PARAMS.clone()
        };
        assert!(params.is_valid_if_initiated_at(0));

        let params = Params {
            swap_due_timestamp_seconds: START_OF_2022_TIMESTAMP_SECONDS
                + Params::MAX_SALE_DURATION_SECONDS,
            sale_delay_seconds: Some(0),
            ..PARAMS.clone()
        };
        assert!(params.is_valid_if_initiated_at(START_OF_2022_TIMESTAMP_SECONDS));

        // Should be invalid with the sale deadline set MAX_SALE_DURATION_SECONDS + 1 second from now.
        let params = Params {
            swap_due_timestamp_seconds: Params::MAX_SALE_DURATION_SECONDS + 1,
            sale_delay_seconds: Some(0),
            ..PARAMS.clone()
        };
        assert!(!params.is_valid_if_initiated_at(0));

        let params = Params {
            swap_due_timestamp_seconds: START_OF_2022_TIMESTAMP_SECONDS
                + Params::MAX_SALE_DURATION_SECONDS
                + 1,
            sale_delay_seconds: Some(0),
            ..PARAMS.clone()
        };
        assert!(!params.is_valid_if_initiated_at(START_OF_2022_TIMESTAMP_SECONDS));
    }

    #[test]
    fn sale_cannot_be_open_more_than_90_days_takes_into_account_delay() {
        // Would normally be invalid with MAX_SALE_DURATION_SECONDS + 1 second, but 1 second
        // of sale_delay makes the real period only MAX_SALE_DURATION_SECONDS, which is allowed.
        let params = Params {
            swap_due_timestamp_seconds: Params::MAX_SALE_DURATION_SECONDS + 1,
            sale_delay_seconds: Some(1),
            ..PARAMS.clone()
        };
        assert!(params.is_valid_if_initiated_at(0));

        let params = Params {
            swap_due_timestamp_seconds: START_OF_2022_TIMESTAMP_SECONDS
                + Params::MAX_SALE_DURATION_SECONDS
                + 1,
            sale_delay_seconds: Some(1),
            ..PARAMS.clone()
        };
        assert!(params.is_valid_if_initiated_at(START_OF_2022_TIMESTAMP_SECONDS));
    }

    #[test]
    fn sale_must_be_open_for_at_least_one_day() {
        // Should be valid with the sale length set to MIN_SALE_DURATION_SECONDS.
        let params = Params {
            swap_due_timestamp_seconds: Params::MIN_SALE_DURATION_SECONDS,
            sale_delay_seconds: Some(0),
            ..PARAMS.clone()
        };
        assert!(params.is_valid_if_initiated_at(0));

        let params = Params {
            swap_due_timestamp_seconds: START_OF_2022_TIMESTAMP_SECONDS
                + Params::MIN_SALE_DURATION_SECONDS,
            sale_delay_seconds: Some(0),
            ..PARAMS.clone()
        };
        assert!(params.is_valid_if_initiated_at(START_OF_2022_TIMESTAMP_SECONDS));

        // Should fail with the sale length set to one second less than MIN_SALE_DURATION_SECONDS.
        let params = Params {
            swap_due_timestamp_seconds: Params::MIN_SALE_DURATION_SECONDS - 1,
            sale_delay_seconds: Some(0),
            ..PARAMS.clone()
        };
        assert!(!params.is_valid_if_initiated_at(0));

        let params = Params {
            swap_due_timestamp_seconds: START_OF_2022_TIMESTAMP_SECONDS
                + Params::MIN_SALE_DURATION_SECONDS
                - 1,
            sale_delay_seconds: Some(0),
            ..PARAMS.clone()
        };
        assert!(!params.is_valid_if_initiated_at(START_OF_2022_TIMESTAMP_SECONDS));
    }

    #[test]
    fn sale_must_be_open_for_at_least_one_day_takes_into_account_delay() {
        // Should be valid with the sale deadline set to MIN_SALE_DURATION_SECONDS + 1 second from now
        // with a sale delay of 1 second.
        let params = Params {
            swap_due_timestamp_seconds: Params::MIN_SALE_DURATION_SECONDS + 1,
            sale_delay_seconds: Some(1),
            ..PARAMS.clone()
        };
        assert!(params.is_valid_if_initiated_at(0));

        let params = Params {
            swap_due_timestamp_seconds: START_OF_2022_TIMESTAMP_SECONDS
                + Params::MIN_SALE_DURATION_SECONDS
                + 1,
            sale_delay_seconds: Some(1),
            ..PARAMS.clone()
        };
        assert!(params.is_valid_if_initiated_at(START_OF_2022_TIMESTAMP_SECONDS));

        // Should be invalid with the sale deadline set to MIN_SALE_DURATION_SECONDS from now
        // with a sale delay of 1 second.
        let params = Params {
            swap_due_timestamp_seconds: Params::MIN_SALE_DURATION_SECONDS,
            sale_delay_seconds: Some(1),
            ..PARAMS.clone()
        };
        assert!(!params.is_valid_if_initiated_at(0));

        let params = Params {
            swap_due_timestamp_seconds: START_OF_2022_TIMESTAMP_SECONDS
                + Params::MIN_SALE_DURATION_SECONDS,
            sale_delay_seconds: Some(1),
            ..PARAMS.clone()
        };
        assert!(!params.is_valid_if_initiated_at(START_OF_2022_TIMESTAMP_SECONDS));
    }
}
